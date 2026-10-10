//! What a structural scan can show a call argument holds ([`ArgShape`], issue
//! #856): enough for the effects pass to rule out the user code a builtin
//! reaches by converting, comparing or counting an object it was handed.
//!
//! An expression qualifies by its form ([`object_free`]). A variable qualifies
//! through [`FrameBindings`]: a flow-insensitive summary of every write the
//! frame makes to it, which holds wherever the frame reads it because no write
//! the frame contains stores anything else.

use std::collections::{HashMap, HashSet};

use std::ops::{Deref, DerefMut};

use mago_syntax::cst::{
    Access, Argument, ArgumentList, ArrayElement, Assignment, AssignmentOperator, BinaryOperator,
    Call, Construct, Expression, FunctionLikeParameterList, Hint, Literal, Node,
    UnaryPrefixOperator, Variable,
};

use crate::ast::{
    ArgShape, ArgValue, ConstInit, ConstRef, EffectRecv, FloatEvidence, NotText, NullEvidence,
    SUPERGLOBALS, Stored,
};
use crate::lower_effect::EffectScanCx;
use crate::lower_expr::{
    class_const_name, effect_recv_of_class, effect_recv_of_object, effect_recv_of_object_declared,
    lower_int_literal, method_name_of, prop_fetch_of, trace_static_class,
};
use crate::names::name_ref;
use crate::{bytes_to_string, children, strip_dollar};

/// The per-position [`ArgShape`] of a call's arguments, or `None` for a named
/// or spread argument list, whose positions cannot be read.
pub(crate) fn arg_shapes_of(
    list: &ArgumentList<'_>,
    cx: &EffectScanCx,
) -> Option<Vec<ArgShape>> {
    let mut shapes = Vec::new();
    for arg in list.arguments.iter() {
        match arg {
            Argument::Positional(p) if p.ellipsis.is_none() => {
                shapes.push(arg_shape(p.value, cx));
            }
            _ => return None,
        }
    }
    Some(shapes)
}

/// The argument shapes an `EffectOrigin::MethodCall` carries: those of a call
/// whose callee the effects pass can resolve to read a parameter's
/// by-reference flag ([`method_callee_resolvable`]).
pub(crate) fn method_call_shapes(
    receiver: &Expression<'_>,
    list: &ArgumentList<'_>,
    cx: &EffectScanCx,
) -> Option<Vec<ArgShape>> {
    if !method_callee_resolvable(receiver) {
        return None;
    }
    arg_shapes_of(list, cx)
}

/// Whether a method or static call's receiver names its callee to the effects
/// pass by class: `$this`, `self`, `parent` or a class name. Every argument of
/// any other method call counts as written ([`collect_stores`]).
fn method_callee_resolvable(receiver: &Expression<'_>) -> bool {
    match receiver.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            strip_dollar(bytes_to_string(dv.name)) == "this"
        }
        Expression::Identifier(_) | Expression::Self_(_) | Expression::Parent(_) => true,
        _ => false,
    }
}

/// One argument expression's [`ArgShape`].
pub(crate) fn arg_shape(expr: &Expression<'_>, cx: &EffectScanCx) -> ArgShape {
    match expr.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            cx.bindings.shape(&strip_dollar(bytes_to_string(dv.name)))
        }
        Expression::Call(call) => call_shape(call, cx),
        // `Foo::class` is a string and an object-free form; every other class constant is not
        // shown, but only an enum case among them is an object.
        Expression::Access(Access::ClassConstant(cc)) if !object_free(expr) => {
            match (trace_static_class(cc.class), class_const_name(&cc.constant)) {
                (Some(class), Some(name)) => ArgShape::ClassConst { class, name },
                _ => ArgShape::Unknown,
            }
        }
        Expression::ConstantAccess(ca) => ArgShape::GlobalConst(name_ref(&ca.name)),
        Expression::Access(Access::Property(pa)) => match prop_fetch_of(pa.object, &pa.property) {
            Some((var, prop)) if var == "this" => ArgShape::ThisProperty(prop),
            _ => ArgShape::Unknown,
        },
        e => match stored_of(e) {
            Some(Stored::ObjectFree) => ArgShape::ObjectFree,
            Some(Stored::Array) => ArgShape::Array,
            None => ArgShape::Unknown,
        },
    }
}

/// The shape of a call's result: the callee, for the engine to read what its
/// declared return holds ([`ArgShape::Call`], [`ArgShape::MethodCall`]). A call the
/// scan cannot name (`$f()`, `$o->$m()`, `?->`) is [`ArgShape::Unknown`]. The callee's
/// arguments are not read: the declared return holds whatever they are.
fn call_shape(call: &Call<'_>, cx: &EffectScanCx) -> ArgShape {
    let method = |receiver, selector| match (receiver, method_name_of(selector)) {
        (Some(receiver), Some(method)) => ArgShape::MethodCall { receiver, method },
        _ => ArgShape::Unknown,
    };
    match call {
        Call::Function(fc) => match fc.function {
            Expression::Identifier(id) => ArgShape::Call(name_ref(id)),
            _ => ArgShape::Unknown,
        },
        Call::Method(mc) => method(effect_recv_of_object_declared(mc.object, cx), &mc.method),
        Call::StaticMethod(sc) => method(effect_recv_of_class(sc.class), &sc.method),
        Call::NullSafeMethod(_) => ArgShape::Unknown,
    }
}

/// The shape of the container of an offset write ([`FrameBindings::container_shape`]):
/// a bare variable, or `$this->name` for the engine to read the property's declared
/// type. Any other container (an element, a call, another object's property) is not
/// shown to be a non-string.
pub(crate) fn container_shape_of(expr: &Expression<'_>, frame: &FrameBindings) -> ArgShape {
    match expr.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            frame.container_shape(&strip_dollar(bytes_to_string(dv.name)))
        }
        Expression::Access(Access::Property(pa)) => match prop_fetch_of(pa.object, &pa.property) {
            Some((var, prop)) if var == "this" => ArgShape::ThisProperty(prop),
            _ => ArgShape::Unknown,
        },
        _ => ArgShape::Unknown,
    }
}

/// What an expression's value is shown to hold by its form alone: no object
/// at any depth ([`object_free`]), an array that may hold one (an array
/// literal or an `(array)` cast), or nothing known.
fn stored_of(expr: &Expression<'_>) -> Option<Stored> {
    match expr.unparenthesized() {
        e if object_free(e) => Some(Stored::ObjectFree),
        Expression::Array(_) | Expression::LegacyArray(_) => Some(Stored::Array),
        Expression::UnaryPrefix(u) if matches!(u.operator, UnaryPrefixOperator::ArrayCast(..)) => {
            Some(Stored::Array)
        }
        _ => None,
    }
}

/// Whether an expression's value holds no object at any depth, whatever its
/// operands hold ([`ArgShape::ObjectFree`]). Arithmetic and the bitwise
/// operators are left out: GMP and `BcMath\Number` overload them to return an
/// object.
pub(crate) fn object_free(expr: &Expression<'_>) -> bool {
    match expr.unparenthesized() {
        Expression::Literal(_) | Expression::MagicConstant(_) | Expression::CompositeString(_) => {
            true
        }
        Expression::Construct(Construct::Isset(_) | Construct::Empty(_)) => true,
        Expression::Binary(b) => match b.operator {
            BinaryOperator::NullCoalesce(_) => object_free(b.lhs) && object_free(b.rhs),
            BinaryOperator::StringConcat(_)
            | BinaryOperator::Equal(_)
            | BinaryOperator::NotEqual(_)
            | BinaryOperator::Identical(_)
            | BinaryOperator::NotIdentical(_)
            | BinaryOperator::AngledNotEqual(_)
            | BinaryOperator::LessThan(_)
            | BinaryOperator::LessThanOrEqual(_)
            | BinaryOperator::GreaterThan(_)
            | BinaryOperator::GreaterThanOrEqual(_)
            | BinaryOperator::Spaceship(_)
            | BinaryOperator::Instanceof(_)
            | BinaryOperator::And(_)
            | BinaryOperator::Or(_)
            | BinaryOperator::LowAnd(_)
            | BinaryOperator::LowOr(_)
            | BinaryOperator::LowXor(_) => true,
            _ => false,
        },
        Expression::UnaryPrefix(u) => match u.operator {
            UnaryPrefixOperator::ErrorControl(_) => object_free(u.operand),
            UnaryPrefixOperator::Not(_)
            | UnaryPrefixOperator::BoolCast(..)
            | UnaryPrefixOperator::BooleanCast(..)
            | UnaryPrefixOperator::IntCast(..)
            | UnaryPrefixOperator::IntegerCast(..)
            | UnaryPrefixOperator::FloatCast(..)
            | UnaryPrefixOperator::DoubleCast(..)
            | UnaryPrefixOperator::StringCast(..)
            | UnaryPrefixOperator::BinaryCast(..) => true,
            _ => false,
        },
        Expression::Conditional(c) => {
            c.then.map_or_else(|| object_free(c.condition), |t| object_free(t))
                && object_free(c.r#else)
        }
        Expression::Array(a) => a.elements.iter().all(element_object_free),
        Expression::LegacyArray(a) => a.elements.iter().all(element_object_free),
        // A `match` yields the result of one arm (or raises, which yields nothing), so it holds
        // an object only where an arm may; a `throw` expression never yields a value.
        Expression::Match(m) => m.arms.iter().all(|arm| object_free(arm.expression())),
        // `Foo::class`, `static::class` and `$o::class` are strings.
        Expression::Access(Access::ClassConstant(cc)) => {
            class_const_name(&cc.constant).is_some_and(|name| name.eq_ignore_ascii_case("class"))
        }
        Expression::Throw(_) => true,
        _ => false,
    }
}

/// [`object_free`] for one array-literal element: a spread or a `&` element
/// never is, since what it brings in is a variable's.
fn element_object_free(element: &ArrayElement<'_>) -> bool {
    match element {
        ArrayElement::KeyValue(kv) => object_free(kv.key) && object_free(kv.value),
        ArrayElement::Value(v) => object_free(v.value),
        ArrayElement::Variadic(_) | ArrayElement::Missing(_) => false,
    }
}

/// Whether an expression's value is **no float by its form alone**, whatever its
/// operands hold ([`FloatEvidence::NoFloat`]): a string, boolean or `null` literal, an
/// integer literal that fits `int` and its negation, a magic constant, an interpolated
/// string, a concatenation, a comparison or logical connective, `!`, `isset`, `empty`, a
/// cast to `int`, `bool`, `string` or `array`, an array literal, and a ternary or `??` whose
/// results are. Arithmetic is left out (`$a + $b` of integers overflows into a float), as is
/// a cast to `float`.
pub(crate) fn no_float_form(expr: &Expression<'_>) -> bool {
    match expr.unparenthesized() {
        Expression::Literal(Literal::Float(_)) => false,
        // An integer literal wider than `int` is a float.
        Expression::Literal(Literal::Integer(li)) => {
            matches!(lower_int_literal(li.raw), ArgValue::Int(_))
        }
        Expression::Literal(_)
        | Expression::MagicConstant(_)
        | Expression::CompositeString(_)
        | Expression::Array(_)
        | Expression::LegacyArray(_)
        | Expression::Construct(Construct::Isset(_) | Construct::Empty(_)) => true,
        Expression::Binary(b) => match b.operator {
            BinaryOperator::NullCoalesce(_) => no_float_form(b.lhs) && no_float_form(b.rhs),
            BinaryOperator::StringConcat(_)
            | BinaryOperator::Equal(_)
            | BinaryOperator::NotEqual(_)
            | BinaryOperator::Identical(_)
            | BinaryOperator::NotIdentical(_)
            | BinaryOperator::AngledNotEqual(_)
            | BinaryOperator::LessThan(_)
            | BinaryOperator::LessThanOrEqual(_)
            | BinaryOperator::GreaterThan(_)
            | BinaryOperator::GreaterThanOrEqual(_)
            | BinaryOperator::Spaceship(_)
            | BinaryOperator::Instanceof(_)
            | BinaryOperator::And(_)
            | BinaryOperator::Or(_)
            | BinaryOperator::LowAnd(_)
            | BinaryOperator::LowOr(_)
            | BinaryOperator::LowXor(_) => true,
            _ => false,
        },
        Expression::UnaryPrefix(u) => match u.operator {
            UnaryPrefixOperator::ErrorControl(_) => no_float_form(u.operand),
            // `-1` is an integer; `-$x` is not shown.
            UnaryPrefixOperator::Negation(_) => matches!(
                u.operand.unparenthesized(),
                Expression::Literal(Literal::Integer(_))
            ) && no_float_form(u.operand),
            UnaryPrefixOperator::Not(_)
            | UnaryPrefixOperator::BoolCast(..)
            | UnaryPrefixOperator::BooleanCast(..)
            | UnaryPrefixOperator::IntCast(..)
            | UnaryPrefixOperator::IntegerCast(..)
            | UnaryPrefixOperator::StringCast(..)
            | UnaryPrefixOperator::BinaryCast(..)
            | UnaryPrefixOperator::ArrayCast(..) => true,
            _ => false,
        },
        Expression::Conditional(c) => {
            c.then.map_or_else(|| no_float_form(c.condition), |t| no_float_form(t))
                && no_float_form(c.r#else)
        }
        _ => false,
    }
}

/// Whether an expression's value is a float by its form alone ([`FloatEvidence::Float`]): a
/// float literal, an integer literal wider than `int`, a cast to `float`, and the negation of
/// one.
fn float_form(expr: &Expression<'_>) -> bool {
    match expr.unparenthesized() {
        Expression::Literal(Literal::Float(_)) => true,
        Expression::Literal(Literal::Integer(li)) => {
            !matches!(lower_int_literal(li.raw), ArgValue::Int(_))
        }
        Expression::UnaryPrefix(u) => match u.operator {
            UnaryPrefixOperator::FloatCast(..)
            | UnaryPrefixOperator::DoubleCast(..)
            | UnaryPrefixOperator::RealCast(..) => true,
            UnaryPrefixOperator::Negation(_) | UnaryPrefixOperator::ErrorControl(_) => {
                float_form(u.operand)
            }
            _ => false,
        },
        _ => false,
    }
}

/// [`ConstArgs::float_evidence`] of a `sprintf` or `printf` call: the evidence of each
/// argument from position 1 on ([`float_evidence`]). Empty for a named or spread argument
/// list, whose positions cannot be read.
///
/// [`ConstArgs::float_evidence`]: crate::ast::ConstArgs::float_evidence
pub(crate) fn float_evidence_of_args(
    list: &ArgumentList<'_>,
    cx: &EffectScanCx,
) -> Vec<(u8, FloatEvidence)> {
    let mut out = Vec::new();
    for (position, arg) in list.arguments.iter().enumerate() {
        let Argument::Positional(p) = arg else { return Vec::new() };
        if p.ellipsis.is_some() {
            return Vec::new();
        }
        let Ok(position) = u8::try_from(position) else { break };
        if position > 0
            && let Some(evidence) = float_evidence(p.value, Some(cx))
        {
            out.push((position, evidence));
        }
    }
    out
}

/// [`ConstArgs::timestamps`] of a time-family call: the [`NullEvidence`] of each of the first
/// six arguments. Empty for a named or spread argument list, whose positions cannot be read.
///
/// [`ConstArgs::timestamps`]: crate::ast::ConstArgs::timestamps
pub(crate) fn null_evidence_of_args(
    list: &ArgumentList<'_>,
    cx: &EffectScanCx,
) -> Vec<(u8, NullEvidence)> {
    let mut out = Vec::new();
    for (position, arg) in list.arguments.iter().enumerate() {
        let Argument::Positional(p) = arg else { return Vec::new() };
        if p.ellipsis.is_some() {
            return Vec::new();
        }
        let Ok(position) = u8::try_from(position) else { break };
        if position > 5 {
            break;
        }
        out.extend(null_evidence(p.value, cx).map(|evidence| (position, evidence)));
    }
    out
}

/// [`ConstArgs::rendered`] of a call to a function that renders its arguments through the
/// `precision` ini: the [`rendered_evidence`] of every positional argument, in position order.
/// Empty for a named or spread argument list, whose positions cannot be read.
///
/// [`ConstArgs::rendered`]: crate::ast::ConstArgs::rendered
pub(crate) fn rendered_evidence_of_args(
    list: &ArgumentList<'_>,
    cx: &EffectScanCx,
) -> Vec<(u8, FloatEvidence)> {
    let mut out = Vec::new();
    for (position, arg) in list.arguments.iter().enumerate() {
        let Argument::Positional(p) = arg else { return Vec::new() };
        if p.ellipsis.is_some() {
            return Vec::new();
        }
        let Ok(position) = u8::try_from(position) else { break };
        out.extend(rendered_evidence(p.value, cx).map(|evidence| (position, evidence)));
    }
    out
}

/// What the scan shows of whether `expr` is a float **or holds one** ([`FloatEvidence`] read at
/// the depth a renderer walks): [`float_evidence`] for a scalar form, and for an array literal
/// the evidence of its elements ([`FloatEvidence::Members`]), since the `NoFloat` of
/// [`float_evidence`] says the array is no float and nothing of what it holds. A conditional and
/// a `??` are one of their branches, `@` is its operand, and `(array)` of a float is an array
/// holding it (a literal array stays itself, and the cast of anything else shows nothing). An
/// array with a spread element, or a form that is none of these and that [`float_evidence`]
/// cannot name, shows nothing.
///
/// A variable is read as [`float_evidence`] reads it. That evidence says a write of the frame
/// leaves no float in the variable, and an array literal written to it is such a write, so the
/// engine reads a variable's evidence for what it holds only while the frame never writes it.
fn rendered_evidence(expr: &Expression<'_>, cx: &EffectScanCx) -> Option<FloatEvidence> {
    let e = expr.unparenthesized();
    match e {
        Expression::Array(a) => members_evidence(a.elements.iter(), cx),
        Expression::LegacyArray(a) => members_evidence(a.elements.iter(), cx),
        Expression::Conditional(c) => Some(FloatEvidence::OneOf(vec![
            rendered_evidence(c.then.unwrap_or(c.condition), cx)?,
            rendered_evidence(c.r#else, cx)?,
        ])),
        Expression::Binary(b) if matches!(b.operator, BinaryOperator::NullCoalesce(_)) => {
            Some(FloatEvidence::OneOf(vec![
                rendered_evidence(b.lhs, cx)?,
                rendered_evidence(b.rhs, cx)?,
            ]))
        }
        Expression::UnaryPrefix(u) => match u.operator {
            UnaryPrefixOperator::ErrorControl(_) => rendered_evidence(u.operand, cx),
            // `(array) [..]` is the array, `(array) 1.5` an array holding the float, and the
            // elements of the array a variable holds are not what the variable's evidence says.
            UnaryPrefixOperator::ArrayCast(..) => match u.operand.unparenthesized() {
                Expression::Array(_) | Expression::LegacyArray(_) => {
                    rendered_evidence(u.operand, cx)
                }
                operand if float_form(operand) => {
                    Some(FloatEvidence::Members(vec![FloatEvidence::Float]))
                }
                _ => None,
            },
            _ => float_evidence(e, Some(cx)),
        },
        _ => float_evidence(e, Some(cx)),
    }
}

/// [`FloatEvidence::Members`] of the elements of an array literal, or `None` when one is a spread
/// or a hole or shows nothing.
fn members_evidence<'a>(
    elements: impl Iterator<Item = &'a ArrayElement<'a>>,
    cx: &EffectScanCx,
) -> Option<FloatEvidence> {
    let mut members = Vec::new();
    for element in elements {
        let value = match element {
            ArrayElement::KeyValue(kv) => kv.value,
            ArrayElement::Value(v) => v.value,
            ArrayElement::Variadic(_) | ArrayElement::Missing(_) => return None,
        };
        members.push(rendered_evidence(value, cx)?);
    }
    Some(FloatEvidence::Members(members))
}

/// What the scan shows of whether `expr` is `null` ([`NullEvidence`]), or `None` when it shows
/// nothing. A local variable shows nothing: it starts `null`, and the scan does not follow which
/// of its writes a read sees. A `??` is its right side, since a left side that is set is not
/// `null`; a short `?:` is its else branch, since a truthy then branch is not `null` either.
fn null_evidence(expr: &Expression<'_>, cx: &EffectScanCx) -> Option<NullEvidence> {
    let e = expr.unparenthesized();
    if matches!(e, Expression::Literal(Literal::Null(_))) {
        return Some(NullEvidence::MayNull);
    }
    if non_null_form(e) {
        return Some(NullEvidence::NonNull);
    }
    match e {
        Expression::Binary(b) if matches!(b.operator, BinaryOperator::NullCoalesce(_)) => {
            null_evidence(b.rhs, cx)
        }
        Expression::Conditional(c) => match c.then {
            Some(then) => Some(NullEvidence::OneOf(vec![
                null_evidence(then, cx)?,
                null_evidence(c.r#else, cx)?,
            ])),
            None => null_evidence(c.r#else, cx),
        },
        _ => match float_evidence(e, Some(cx))? {
            FloatEvidence::Shape { shape: shape @ ArgShape::Param { .. }, unwritten: true, .. }
            | FloatEvidence::Shape {
                shape:
                    shape @ (ArgShape::ThisProperty(_)
                    | ArgShape::Call(_)
                    | ArgShape::MethodCall { .. }),
                ..
            } => Some(NullEvidence::Shape(shape)),
            _ => None,
        },
    }
}

/// Whether an expression's value is **not `null` by its form alone** ([`NullEvidence::NonNull`]).
fn non_null_form(expr: &Expression<'_>) -> bool {
    match expr.unparenthesized() {
        Expression::Literal(Literal::Null(_)) => false,
        Expression::Literal(_)
        | Expression::MagicConstant(_)
        | Expression::CompositeString(_)
        | Expression::Array(_)
        | Expression::LegacyArray(_)
        | Expression::Instantiation(_)
        | Expression::Construct(Construct::Isset(_) | Construct::Empty(_)) => true,
        Expression::Binary(b) => !matches!(b.operator, BinaryOperator::NullCoalesce(_)),
        Expression::UnaryPrefix(u) => match u.operator {
            UnaryPrefixOperator::ErrorControl(_) => non_null_form(u.operand),
            UnaryPrefixOperator::ArrayCast(..)
            | UnaryPrefixOperator::BoolCast(..)
            | UnaryPrefixOperator::BooleanCast(..)
            | UnaryPrefixOperator::DoubleCast(..)
            | UnaryPrefixOperator::RealCast(..)
            | UnaryPrefixOperator::FloatCast(..)
            | UnaryPrefixOperator::IntCast(..)
            | UnaryPrefixOperator::IntegerCast(..)
            | UnaryPrefixOperator::ObjectCast(..)
            | UnaryPrefixOperator::StringCast(..)
            | UnaryPrefixOperator::BinaryCast(..)
            | UnaryPrefixOperator::BitwiseNot(_)
            | UnaryPrefixOperator::Not(_)
            | UnaryPrefixOperator::Plus(_)
            | UnaryPrefixOperator::Negation(_) => true,
            _ => false,
        },
        _ => false,
    }
}

/// [`ConstArgs::not_text`] of a `ctype_*` call: the first argument, when it is a `null`, boolean,
/// float or array literal, a `new` expression (an object), or a by-value parameter the frame
/// never writes (whose declared type the engine reads). Empty for anything else, and for a named
/// or spread argument list.
///
/// [`ConstArgs::not_text`]: crate::ast::ConstArgs::not_text
pub(crate) fn not_text_of_args(list: &ArgumentList<'_>, cx: &EffectScanCx) -> Vec<(u8, NotText)> {
    let mut args = list.arguments.iter();
    let Some(Argument::Positional(first)) = args.next() else { return Vec::new() };
    if first.ellipsis.is_some() || args.any(|a| !matches!(a, Argument::Positional(p) if p.ellipsis.is_none())) {
        return Vec::new();
    }
    let evidence = match first.value.unparenthesized() {
        Expression::Literal(
            Literal::Null(_) | Literal::True(_) | Literal::False(_) | Literal::Float(_),
        )
        | Expression::Array(_)
        | Expression::LegacyArray(_)
        | Expression::Instantiation(_) => NotText::Literal,
        Expression::Variable(Variable::Direct(dv)) => {
            let name = strip_dollar(bytes_to_string(dv.name));
            if !cx.bindings.unrebound_param(&name) {
                return Vec::new();
            }
            NotText::Param(name)
        }
        _ => return Vec::new(),
    };
    vec![(0, evidence)]
}

/// What the scan shows of whether `expr` is a float ([`FloatEvidence`]), or `None` when it
/// shows nothing. With `cx` the frame's variables and every receiver the effects pass can
/// name are read; without it (a value written to a variable, read while the frame's own
/// bindings are still being built) only the forms that need no frame are.
pub(crate) fn float_evidence(
    expr: &Expression<'_>,
    cx: Option<&EffectScanCx>,
) -> Option<FloatEvidence> {
    let e = expr.unparenthesized();
    if no_float_form(e) {
        return Some(FloatEvidence::NoFloat);
    }
    if float_form(e) {
        return Some(FloatEvidence::Float);
    }
    let shape = |shape| Some(FloatEvidence::Shape { shape, unwritten: false, writes: Vec::new() });
    match e {
        Expression::Variable(Variable::Direct(dv)) => {
            cx?.bindings.float_shape(&strip_dollar(bytes_to_string(dv.name)))
        }
        Expression::Call(call) => {
            let called = match cx {
                Some(cx) => call_shape(call, cx),
                None => call_shape_without_frame(call),
            };
            (called != ArgShape::Unknown).then_some(called).and_then(shape)
        }
        Expression::Access(Access::Property(pa)) => match prop_fetch_of(pa.object, &pa.property) {
            Some((var, prop)) if var == "this" => shape(ArgShape::ThisProperty(prop)),
            _ => None,
        },
        Expression::Access(Access::StaticProperty(sp)) => {
            let Variable::Direct(dv) = &sp.property else { return None };
            Some(FloatEvidence::StaticProperty {
                class: trace_static_class(sp.class)?,
                name: strip_dollar(bytes_to_string(dv.name)),
            })
        }
        Expression::Access(Access::ClassConstant(cc)) => Some(FloatEvidence::ClassConst {
            class: trace_static_class(cc.class)?,
            name: class_const_name(&cc.constant)?,
        }),
        Expression::ConstantAccess(ca) => Some(FloatEvidence::GlobalConst(name_ref(&ca.name))),
        Expression::Conditional(c) => Some(FloatEvidence::OneOf(vec![
            float_evidence(c.then.unwrap_or(c.condition), cx)?,
            float_evidence(c.r#else, cx)?,
        ])),
        Expression::Binary(b) if matches!(b.operator, BinaryOperator::NullCoalesce(_)) => {
            Some(FloatEvidence::OneOf(vec![
                float_evidence(b.lhs, cx)?,
                float_evidence(b.rhs, cx)?,
            ]))
        }
        _ => None,
    }
}

/// [`call_shape`] for a call whose callee needs no frame to name: a function, a static call
/// on a class, `self` or `parent`, and `$this->m()`.
fn call_shape_without_frame(call: &Call<'_>) -> ArgShape {
    match call {
        Call::Function(fc) => match fc.function {
            Expression::Identifier(id) => ArgShape::Call(name_ref(id)),
            _ => ArgShape::Unknown,
        },
        Call::Method(mc) => match (effect_recv_of_object(mc.object), method_name_of(&mc.method)) {
            (Some(receiver @ EffectRecv::This), Some(method)) => {
                ArgShape::MethodCall { receiver, method }
            }
            _ => ArgShape::Unknown,
        },
        Call::StaticMethod(sc) => {
            match (effect_recv_of_class(sc.class), method_name_of(&sc.method)) {
                (Some(receiver), Some(method)) => ArgShape::MethodCall { receiver, method },
                _ => ArgShape::Unknown,
            }
        }
        Call::NullSafeMethod(_) => ArgShape::Unknown,
    }
}

/// The variables of one frame an argument can name as an [`ArgShape::Param`]
/// or an [`ArgShape::Local`], with what every write the frame makes to each
/// stores. Built once per frame; an aliasing frame (`global`, `static`, `$$v`,
/// `extract`, `include`, a reference assignment or a by-ref capture) shows
/// nothing, since any name there may be rebound unseen.
#[derive(Debug, Default)]
pub(crate) struct FrameBindings {
    /// The by-value, non-variadic parameters.
    params: HashSet<String>,
    /// Variables whose first binding the frame does not make: a by-ref or
    /// variadic parameter and a closure's `use` capture.
    imported: HashSet<String>,
    /// An arrow function captures every free variable from its parent.
    captures_all: bool,
    /// The meet of what each write stores, per variable, and the variables some
    /// whole-variable write may leave a string.
    stores: Writes,
    /// The by-value parameters whose declared type admits no string.
    non_string_params: HashSet<String>,
    /// An aliasing frame, or one this summary was never built for.
    opaque: bool,
}

impl FrameBindings {
    /// The summary for a frame nothing is shown about.
    pub(crate) fn opaque() -> Self {
        Self { opaque: true, ..Self::default() }
    }

    /// Summarize a frame: its parameters, the variables a closure imports
    /// (`uses`) or, for an arrow function, that it captures them all, and
    /// every write its body makes.
    pub(crate) fn new<'a, 'arena: 'a>(
        params: &FunctionLikeParameterList<'_>,
        captures: Captures<'_>,
        body: impl Iterator<Item = Node<'a, 'arena>>,
    ) -> Self {
        let mut frame = Self::default();
        for p in params.parameters.iter() {
            let name = strip_dollar(bytes_to_string(p.variable.name));
            if p.is_reference() || p.ellipsis.is_some() {
                frame.imported.insert(name);
            } else {
                if p.hint.as_ref().is_some_and(non_string_hint) {
                    frame.non_string_params.insert(name.clone());
                }
                frame.params.insert(name);
            }
        }
        match captures {
            Captures::None => {}
            Captures::Uses(names) => frame.imported.extend(names.iter().cloned()),
            Captures::All => frame.captures_all = true,
        }
        for node in body {
            collect_stores(&node, &mut frame.stores);
        }
        frame
    }

    /// Whether `name` is a by-value, non-variadic parameter that no statement of
    /// the frame writes: its value is what the caller bound, in every statement.
    /// An aliasing frame shows nothing, and a write of any kind (an assignment,
    /// `++`, a `foreach` binding, `unset`) rules the name out.
    pub(crate) fn unrebound_param(&self, name: &str) -> bool {
        !self.opaque && self.params.contains(name) && !self.stores.contains_key(name)
    }

    /// The shape of the container of an offset write `$name[…] = …`: a variable
    /// the frame never leaves a string, so an offset write into it stores an
    /// element (or calls `offsetSet`) and converts nothing. A parameter
    /// qualifies by its declared type, which admits no `string`, `mixed` or
    /// `callable`; a local by its writes, each an array, `null`, a number, a
    /// boolean or an object (`new`, a closure, a cast to one of those), and the
    /// union `+=` of an array. Anything else is [`ArgShape::Unknown`]: a string
    /// container converts the value it is handed (`$s[0] = $o`).
    ///
    /// The shape is [`ArgShape::Param`] or [`ArgShape::Local`] with
    /// [`Stored::Array`], whatever the variable holds, so that the engine's
    /// by-reference check still applies (`Frame::container_not_string`): only
    /// this function builds the shape of an offset write's container, and the
    /// engine reads no other as a non-string one.
    pub(crate) fn container_shape(&self, name: &str) -> ArgShape {
        let foreign = name == "this" || SUPERGLOBALS.contains(&name);
        if self.opaque
            || foreign
            || self.imported.contains(name)
            || self.stores.maybe_string.contains(name)
        {
            return ArgShape::Unknown;
        }
        if self.params.contains(name) {
            let declared = self.non_string_params.contains(name);
            declared.then(|| ArgShape::Param { name: name.to_owned(), stores: Stored::Array })
        } else if self.captures_all {
            None
        } else {
            Some(ArgShape::Local { name: name.to_owned(), stores: Stored::Array })
        }
        .unwrap_or(ArgShape::Unknown)
    }

    /// The evidence of a bare `$name` argument for whether it is a float
    /// ([`FloatEvidence::Shape`]): a by-value parameter or a local of the frame, while no
    /// write of the frame may leave a float in it that the scan cannot name. The shape is
    /// [`ArgShape::Param`] or [`ArgShape::Local`] (its `stores` is a placeholder: the claim is
    /// not about objects); the parameter's declared type and the by-reference question are
    /// the engine's to read, as for any variable shape.
    pub(crate) fn float_shape(&self, name: &str) -> Option<FloatEvidence> {
        let foreign = name == "this" || SUPERGLOBALS.contains(&name);
        if self.opaque
            || foreign
            || self.imported.contains(name)
            || self.stores.maybe_float.contains(name)
        {
            return None;
        }
        let stores = Stored::ObjectFree;
        let shape = if self.params.contains(name) {
            ArgShape::Param { name: name.to_owned(), stores }
        } else if self.captures_all {
            return None;
        } else {
            ArgShape::Local { name: name.to_owned(), stores }
        };
        Some(FloatEvidence::Shape {
            shape,
            unwritten: !self.stores.contains_key(name),
            writes: self.stores.carried.get(name).cloned().unwrap_or_default(),
        })
    }

    /// The shape of a bare `$name` argument.
    pub(crate) fn shape(&self, name: &str) -> ArgShape {
        let foreign = name == "this" || SUPERGLOBALS.contains(&name);
        if self.opaque || foreign || self.imported.contains(name) {
            return ArgShape::Unknown;
        }
        let stores = match self.stores.get(name) {
            Some(None) => return ArgShape::Unknown,
            Some(Some(stored)) => *stored,
            None => Stored::ObjectFree,
        };
        if self.params.contains(name) {
            ArgShape::Param { name: name.to_owned(), stores }
        } else if self.captures_all {
            ArgShape::Unknown
        } else {
            ArgShape::Local { name: name.to_owned(), stores }
        }
    }
}

/// What each variable's writes store, met over every write: `None` once some
/// write stores a value nothing is shown about ([`FrameBindings`]).
type Stores = HashMap<String, Option<Stored>>;

/// The writes a frame makes: what each variable stores ([`Stores`], which a
/// `Writes` derefs to), the variables some whole-variable write may leave a
/// string (`$v = 'abc'`, `$v = f()`, `$v .= 'x'`, a destructuring target), which
/// [`FrameBindings::container_shape`] reads, and the variables some write may leave
/// a float, which [`FrameBindings::float_shape`] reads.
#[derive(Debug, Default)]
struct Writes {
    stores: Stores,
    maybe_string: HashSet<String>,
    /// The variables some write may leave a float in ([`FrameBindings::float_shape`]).
    /// A write counts unless the scan shows its value is no float or can name what it is,
    /// so a write the scan cannot read is in here by default.
    maybe_float: HashSet<String>,
    /// The evidence of every whole-variable write the scan can name that is not a plain
    /// no-float form (a call result, a ternary, a constant, a float), per variable.
    carried: HashMap<String, Vec<FloatEvidence>>,
}

impl Deref for Writes {
    type Target = Stores;

    fn deref(&self) -> &Stores {
        &self.stores
    }
}

impl DerefMut for Writes {
    fn deref_mut(&mut self) -> &mut Stores {
        &mut self.stores
    }
}

/// Whether the declared type `hint` admits no string: every member is a class,
/// `array`, `iterable`, `object`, a number, a boolean or `null`. `string`,
/// `mixed` and `callable` (a function name is a string) may hold one.
fn non_string_hint(hint: &Hint<'_>) -> bool {
    match hint {
        Hint::Identifier(_)
        | Hint::Array(_)
        | Hint::Null(_)
        | Hint::True(_)
        | Hint::False(_)
        | Hint::Static(_)
        | Hint::Self_(_)
        | Hint::Parent(_)
        | Hint::Float(_)
        | Hint::Bool(_)
        | Hint::Integer(_)
        | Hint::Object(_)
        | Hint::Iterable(_)
        | Hint::Intersection(_) => true,
        Hint::Nullable(n) => non_string_hint(n.hint),
        Hint::Parenthesized(p) => non_string_hint(p.hint),
        Hint::Union(u) => non_string_hint(u.left) && non_string_hint(u.right),
        Hint::Callable(_)
        | Hint::Void(_)
        | Hint::Never(_)
        | Hint::String(_)
        | Hint::Mixed(_) => false,
    }
}

/// Whether storing `value` into a variable shows it is no string: an array, an
/// object (`new`, a closure), a number, a boolean or `null`, or a cast to one.
fn string_free_value(value: &Expression<'_>) -> bool {
    match value.unparenthesized() {
        Expression::Array(_)
        | Expression::LegacyArray(_)
        | Expression::Instantiation(_)
        | Expression::Closure(_)
        | Expression::ArrowFunction(_) => true,
        Expression::Literal(literal) => !matches!(literal, Literal::String(_)),
        Expression::UnaryPrefix(u) => matches!(
            u.operator,
            UnaryPrefixOperator::ArrayCast(..)
                | UnaryPrefixOperator::ObjectCast(..)
                | UnaryPrefixOperator::BoolCast(..)
                | UnaryPrefixOperator::BooleanCast(..)
                | UnaryPrefixOperator::IntCast(..)
                | UnaryPrefixOperator::IntegerCast(..)
                | UnaryPrefixOperator::FloatCast(..)
                | UnaryPrefixOperator::DoubleCast(..)
        ),
        _ => false,
    }
}

/// Carry the evidence of a whole-variable write whose value the scan can name but that is
/// no plain no-float form ([`Writes::carried`]); whether the write may leave a float is then
/// the engine's to say. `false` when the target is not a bare variable or the value shows
/// nothing.
fn carry_write(a: &Assignment<'_>, out: &mut Writes) -> bool {
    let Expression::Variable(Variable::Direct(dv)) = a.lhs.unparenthesized() else { return false };
    let Some(evidence) = float_evidence(a.rhs, None) else { return false };
    out.carried.entry(strip_dollar(bytes_to_string(dv.name))).or_default().push(evidence);
    true
}

/// Whether an assignment stores into a bare variable a value shown no string that
/// is neither object-free nor an array literal: an object (`new`, a closure).
fn is_object_write(a: &Assignment<'_>) -> bool {
    matches!(a.operator, AssignmentOperator::Assign(_) | AssignmentOperator::Coalesce(_))
        && matches!(a.lhs.unparenthesized(), Expression::Variable(Variable::Direct(_)))
        && stored_of(a.rhs).is_none()
        && string_free_value(a.rhs)
}

/// Record the variables an assignment may leave a string in
/// ([`Writes::maybe_string`]): a whole-variable target unless its value is shown
/// no string (`=` and `??=`, and `+=` of an array), and every variable of a
/// destructuring pattern.
fn note_maybe_string(a: &Assignment<'_>, out: &mut Writes) {
    match a.lhs.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            let keeps = match a.operator {
                AssignmentOperator::Assign(_)
                | AssignmentOperator::Coalesce(_)
                | AssignmentOperator::Addition(_) => string_free_value(a.rhs),
                _ => false,
            };
            if !keeps {
                out.maybe_string.insert(strip_dollar(bytes_to_string(dv.name)));
            }
        }
        Expression::List(l) => l.elements.iter().for_each(|e| pattern_vars(e, out)),
        Expression::Array(arr) => arr.elements.iter().for_each(|e| pattern_vars(e, out)),
        Expression::LegacyArray(arr) => arr.elements.iter().for_each(|e| pattern_vars(e, out)),
        _ => {}
    }
}

/// Every variable a destructuring pattern element binds, nested patterns included.
fn pattern_vars(element: &ArrayElement<'_>, out: &mut Writes) {
    let target = match element {
        ArrayElement::KeyValue(kv) => kv.value,
        ArrayElement::Value(v) => v.value,
        ArrayElement::Variadic(_) | ArrayElement::Missing(_) => return,
    };
    match target.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            out.maybe_string.insert(strip_dollar(bytes_to_string(dv.name)));
        }
        Expression::List(l) => l.elements.iter().for_each(|e| pattern_vars(e, out)),
        Expression::Array(arr) => arr.elements.iter().for_each(|e| pattern_vars(e, out)),
        Expression::LegacyArray(arr) => arr.elements.iter().for_each(|e| pattern_vars(e, out)),
        _ => {}
    }
}

/// Which variables a frame imports from its parent ([`FrameBindings::new`]).
pub(crate) enum Captures<'a> {
    /// A function or method: none.
    None,
    /// A closure: its `use` list, by value (a by-ref capture aliases the frame).
    Uses(&'a [String]),
    /// An arrow function: every free variable.
    All,
}

/// Fold what `value` holds into everything a variable has been shown to store.
fn record(out: &mut Writes, name: String, value: Option<Stored>) {
    record_with(out, name, value, false);
}

/// [`record`] for a write the scan can say whether it leaves a float: `float_free`
/// is true when the value stored is no float, and a write that does not say so
/// counts as one that may ([`Writes::maybe_float`]).
fn record_with(out: &mut Writes, name: String, value: Option<Stored>, float_free: bool) {
    if !float_free {
        out.maybe_float.insert(name.clone());
    }
    // A write nothing reads (a `foreach` or `catch` binding, a reference) may leave a string.
    if value.is_none() {
        out.maybe_string.insert(name.clone());
    }
    let slot = out.stores.entry(name).or_insert(Some(Stored::ObjectFree));
    *slot = match (*slot, value) {
        (None, _) | (_, None) => None,
        (Some(Stored::Array), _) | (_, Some(Stored::Array)) => Some(Stored::Array),
        (Some(Stored::ObjectFree), Some(Stored::ObjectFree)) => Some(Stored::ObjectFree),
    };
}

/// Record every write a subtree makes ([`FrameBindings::stores`]). A write the
/// scan cannot read stores "anything": a `foreach` or `catch` binding, a
/// destructuring of a value not shown object-free, a write through a
/// reference, and any argument a callee might take by reference other than a
/// bare variable in a named call with positional arguments, which the effects
/// pass checks against the callee it resolves. Nested function-like bodies are
/// frames of their own and are not descended.
fn collect_stores(node: &Node<'_, '_>, out: &mut Writes) {
    match node {
        // A whole-variable write of a value shown no string that no `Stored` names
        // (`$v = new Foo`) stores "anything" for the variable's own shape, and leaves
        // it no string for an offset write's container.
        Node::Assignment(a) if is_object_write(a) => {
            if let Expression::Variable(Variable::Direct(dv)) = a.lhs.unparenthesized() {
                out.stores.insert(strip_dollar(bytes_to_string(dv.name)), None);
            }
        }
        Node::Assignment(a) => {
            note_maybe_string(a, out);
            let value = match a.operator {
                AssignmentOperator::Assign(_) | AssignmentOperator::Coalesce(_) => stored_of(a.rhs),
                AssignmentOperator::Concat(_) => Some(Stored::ObjectFree),
                // Arithmetic keeps a scalar a scalar and unions an array, but an
                // object operand (GMP) makes an object.
                _ => stored_of(a.rhs).filter(|s| *s == Stored::ObjectFree),
            };
            // A string is no float; `=` and `??=` store what the value's form says,
            // and every other compound assignment may leave a float (`+=` of ints
            // overflows into one).
            let float_free = match a.operator {
                AssignmentOperator::Assign(_) | AssignmentOperator::Coalesce(_) => {
                    no_float_form(a.rhs) || carry_write(a, out)
                }
                AssignmentOperator::Concat(_) => true,
                _ => false,
            };
            store_into(a.lhs, value, float_free, out);
        }
        Node::UnaryPrefix(u)
            if matches!(
                u.operator,
                UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_)
            ) =>
        {
            // `++` of the greatest integer is a float.
            store_into(u.operand, Some(Stored::ObjectFree), false, out);
        }
        Node::UnaryPostfix(u) => store_into(u.operand, Some(Stored::ObjectFree), false, out),
        // `unset($v)` leaves `null`; unsetting an element or a property stores nothing.
        Node::Unset(u) => {
            for target in u.values.iter() {
                if let Expression::Variable(Variable::Direct(dv)) = target.unparenthesized() {
                    let name = strip_dollar(bytes_to_string(dv.name));
                    record_with(out, name, Some(Stored::ObjectFree), true);
                }
            }
        }
        // `foreach ($it as &$v)` writes through its subject.
        Node::Foreach(fe) if fe.target.value().is_reference() => {
            store_into(fe.expression, None, false, out);
        }
        Node::ForeachValueTarget(t) => store_into(t.value, None, false, out),
        Node::ForeachKeyValueTarget(t) => {
            store_into(t.key, None, false, out);
            store_into(t.value, None, false, out);
        }
        Node::TryCatchClause(c) => {
            if let Some(v) = &c.variable {
                record(out, strip_dollar(bytes_to_string(v.name)), None);
            }
        }
        Node::FunctionCall(fc) => {
            let named = matches!(fc.function, Expression::Identifier(_));
            store_into_args(&fc.argument_list, named, out);
        }
        Node::MethodCall(c) => {
            let named = method_name_of(&c.method).is_some();
            store_into_args(&c.argument_list, named && method_callee_resolvable(c.object), out);
        }
        Node::NullSafeMethodCall(c) => store_into_args(&c.argument_list, false, out),
        Node::StaticMethodCall(c) => {
            let named = method_name_of(&c.method).is_some();
            store_into_args(&c.argument_list, named && method_callee_resolvable(c.class), out);
        }
        Node::Instantiation(i) => {
            if let Some(list) = &i.argument_list {
                store_into_args(list, trace_static_class(i.class).is_some(), out);
            }
        }
        // The class body is a scope of its own, but its constructor arguments
        // are evaluated, and may be taken by reference, in this frame.
        Node::AnonymousClass(c) => {
            let args = c.argument_list.iter().flat_map(|l| l.arguments.iter());
            for value in args.filter_map(|a| a.value()) {
                store_into_root(value, out);
            }
            return;
        }
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => {}
    }
    for child in children(node) {
        collect_stores(&child, out);
    }
}

/// Record that the assignment target `lhs` receives `value`, `float_free` saying
/// whether that value is shown to be no float. A variable stores
/// it; an element write `$a[…] = …` keeps `$a` an array and stores into it an
/// element holding `value`; a property write rebinds no variable; a
/// destructuring hands each target an element of `value`. Any other target
/// counts every variable in it as storing anything.
fn store_into(lhs: &Expression<'_>, value: Option<Stored>, float_free: bool, out: &mut Writes) {
    match lhs.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            record_with(out, strip_dollar(bytes_to_string(dv.name)), value, float_free);
        }
        // The root stays an array (or a string, for an offset write into one),
        // and gains an element holding `value`. A chain through a property or
        // a call writes into no variable.
        Expression::ArrayAccess(_) | Expression::ArrayAppend(_) => {
            if let Some(root) = element_root(lhs) {
                let stored = value.filter(|s| *s == Stored::ObjectFree).unwrap_or(Stored::Array);
                // The root stays an array or a string (or is an error, for a scalar).
                record_with(out, root, Some(stored), true);
            }
        }
        Expression::Access(
            Access::Property(_) | Access::NullSafeProperty(_) | Access::StaticProperty(_),
        ) => {}
        Expression::List(l) => {
            let element = value.filter(|s| *s == Stored::ObjectFree);
            l.elements.iter().for_each(|e| destructure(e, element, out));
        }
        Expression::Array(a) => {
            let element = value.filter(|s| *s == Stored::ObjectFree);
            a.elements.iter().for_each(|e| destructure(e, element, out));
        }
        _ => store_into_root(lhs, out),
    }
}

/// One element of a destructuring target ([`store_into`]).
fn destructure(element: &ArrayElement<'_>, value: Option<Stored>, out: &mut Writes) {
    match element {
        ArrayElement::KeyValue(kv) => store_into(kv.value, value, false, out),
        ArrayElement::Value(v) => store_into(v.value, value, false, out),
        ArrayElement::Variadic(_) | ArrayElement::Missing(_) => {}
    }
}

/// The variable an element write `$a[…][…] = …` or `$a[] = …` writes into, or
/// `None` when the chain runs through a property or a call.
fn element_root(lhs: &Expression<'_>) -> Option<String> {
    let mut cur = lhs.unparenthesized();
    loop {
        cur = match cur {
            Expression::ArrayAccess(a) => a.array.unparenthesized(),
            Expression::ArrayAppend(a) => a.array.unparenthesized(),
            Expression::Variable(Variable::Direct(dv)) => {
                return Some(strip_dollar(bytes_to_string(dv.name)));
            }
            _ => return None,
        };
    }
}

/// Count the variable at the root of `expr` as storing anything: the variable
/// itself, or the one under an offset or property chain, which a callee
/// taking the argument by reference writes through (`f($a['k'])` can turn a
/// `null` into an array, or store an object into it). A value no reference
/// can bind to has no root.
fn store_into_root(expr: &Expression<'_>, out: &mut Writes) {
    let mut cur = expr.unparenthesized();
    loop {
        cur = match cur {
            Expression::ArrayAccess(a) => a.array.unparenthesized(),
            Expression::ArrayAppend(a) => a.array.unparenthesized(),
            Expression::Access(Access::Property(p)) => p.object.unparenthesized(),
            Expression::Access(Access::NullSafeProperty(p)) => p.object.unparenthesized(),
            _ => break,
        };
    }
    if let Expression::Variable(Variable::Direct(dv)) = cur {
        record(out, strip_dollar(bytes_to_string(dv.name)), None);
    }
}

/// Record the arguments of one call ([`collect_stores`]). Where the effects
/// pass resolves the callee (`resolvable`) and the list is positional, a bare
/// variable is left for it to check against the callee's parameters, through
/// the shapes the call's origin carries; every other argument's root counts as
/// storing anything.
fn store_into_args(list: &ArgumentList<'_>, resolvable: bool, out: &mut Writes) {
    let positional =
        list.arguments.iter().all(|a| matches!(a, Argument::Positional(p) if p.ellipsis.is_none()));
    for arg in list.arguments.iter() {
        let value = arg.value().unparenthesized();
        let bare = matches!(value, Expression::Variable(Variable::Direct(_)));
        if !(resolvable && positional && bare) {
            store_into_root(arg.value(), out);
        }
    }
}

/// What a class constant's initializer can evaluate to ([`ConstInit`]): the constants it takes
/// a value from, and whether any part of it is not a form the scan reads. A literal form, an
/// array of them and `Foo::class` need nothing; arithmetic, bit operations, `??`, `?:` and
/// array elements pass what their operands are through.
pub(crate) fn const_init(expr: &Expression<'_>) -> ConstInit {
    let mut init = ConstInit::default();
    collect_const_init(expr, &mut init, 0);
    init
}

fn collect_const_init(expr: &Expression<'_>, init: &mut ConstInit, depth: u8) {
    if depth > 24 {
        init.opaque = true;
        return;
    }
    let expr = expr.unparenthesized();
    if object_free(expr) {
        return;
    }
    match expr {
        Expression::ConstantAccess(ca) => init.refs.push(ConstRef::Global(name_ref(&ca.name))),
        Expression::Access(Access::ClassConstant(cc)) => {
            match (trace_static_class(cc.class), class_const_name(&cc.constant)) {
                (Some(class), Some(name)) => init.refs.push(ConstRef::Class { class, name }),
                _ => init.opaque = true,
            }
        }
        Expression::Binary(b) => match b.operator {
            BinaryOperator::Addition(_)
            | BinaryOperator::Subtraction(_)
            | BinaryOperator::Multiplication(_)
            | BinaryOperator::Division(_)
            | BinaryOperator::Modulo(_)
            | BinaryOperator::Exponentiation(_)
            | BinaryOperator::BitwiseAnd(_)
            | BinaryOperator::BitwiseOr(_)
            | BinaryOperator::BitwiseXor(_)
            | BinaryOperator::LeftShift(_)
            | BinaryOperator::RightShift(_)
            | BinaryOperator::NullCoalesce(_) => {
                collect_const_init(b.lhs, init, depth + 1);
                collect_const_init(b.rhs, init, depth + 1);
            }
            _ => init.opaque = true,
        },
        Expression::UnaryPrefix(u) => match u.operator {
            UnaryPrefixOperator::Negation(_)
            | UnaryPrefixOperator::Plus(_)
            | UnaryPrefixOperator::BitwiseNot(_) => collect_const_init(u.operand, init, depth + 1),
            _ => init.opaque = true,
        },
        Expression::Conditional(c) => {
            collect_const_init(c.then.unwrap_or(c.condition), init, depth + 1);
            collect_const_init(c.r#else, init, depth + 1);
        }
        Expression::Array(a) => a.elements.iter().for_each(|e| collect_element(e, init, depth)),
        Expression::LegacyArray(a) => {
            a.elements.iter().for_each(|e| collect_element(e, init, depth));
        }
        _ => init.opaque = true,
    }
}

fn collect_element(element: &ArrayElement<'_>, init: &mut ConstInit, depth: u8) {
    match element {
        ArrayElement::KeyValue(kv) => {
            collect_const_init(kv.key, init, depth + 1);
            collect_const_init(kv.value, init, depth + 1);
        }
        ArrayElement::Value(v) => collect_const_init(v.value, init, depth + 1),
        ArrayElement::Variadic(v) => collect_const_init(v.value, init, depth + 1),
        ArrayElement::Missing(_) => {}
    }
}
