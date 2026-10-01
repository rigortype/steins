//! What a structural scan can show a call argument holds ([`ArgShape`], issue
//! #856): enough for the effects pass to rule out the user code a builtin
//! reaches by converting, comparing or counting an object it was handed.
//!
//! An expression qualifies by its form ([`object_free`]). A variable qualifies
//! through [`FrameBindings`]: a flow-insensitive summary of every write the
//! frame makes to it, which holds wherever the frame reads it because no write
//! the frame contains stores anything else.

use std::collections::{HashMap, HashSet};

use mago_syntax::cst::{
    Access, Argument, ArgumentList, ArrayElement, AssignmentOperator, BinaryOperator, Construct,
    Expression, FunctionLikeParameterList, Node, UnaryPrefixOperator, Variable,
};

use crate::ast::{ArgShape, SUPERGLOBALS, Stored};
use crate::lower_expr::{method_name_of, prop_fetch_of, trace_static_class};
use crate::{bytes_to_string, children, strip_dollar};

/// The per-position [`ArgShape`] of a call's arguments, or `None` for a named
/// or spread argument list, whose positions cannot be read.
pub(crate) fn arg_shapes_of(
    list: &ArgumentList<'_>,
    frame: &FrameBindings,
) -> Option<Vec<ArgShape>> {
    let mut shapes = Vec::new();
    for arg in list.arguments.iter() {
        match arg {
            Argument::Positional(p) if p.ellipsis.is_none() => {
                shapes.push(arg_shape(p.value, frame));
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
    frame: &FrameBindings,
) -> Option<Vec<ArgShape>> {
    if !method_callee_resolvable(receiver) {
        return None;
    }
    arg_shapes_of(list, frame)
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
pub(crate) fn arg_shape(expr: &Expression<'_>, frame: &FrameBindings) -> ArgShape {
    match expr.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            frame.shape(&strip_dollar(bytes_to_string(dv.name)))
        }
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
    /// The meet of what each write stores, per variable.
    stores: Stores,
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

    /// The shape of a bare `$name` argument.
    fn shape(&self, name: &str) -> ArgShape {
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
fn record(stores: &mut Stores, name: String, value: Option<Stored>) {
    let slot = stores.entry(name).or_insert(Some(Stored::ObjectFree));
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
fn collect_stores(node: &Node<'_, '_>, out: &mut Stores) {
    match node {
        Node::Assignment(a) => {
            let value = match a.operator {
                AssignmentOperator::Assign(_) | AssignmentOperator::Coalesce(_) => stored_of(a.rhs),
                AssignmentOperator::Concat(_) => Some(Stored::ObjectFree),
                // Arithmetic keeps a scalar a scalar and unions an array, but an
                // object operand (GMP) makes an object.
                _ => stored_of(a.rhs).filter(|s| *s == Stored::ObjectFree),
            };
            store_into(a.lhs, value, out);
        }
        Node::UnaryPrefix(u)
            if matches!(
                u.operator,
                UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_)
            ) =>
        {
            store_into(u.operand, Some(Stored::ObjectFree), out);
        }
        Node::UnaryPostfix(u) => store_into(u.operand, Some(Stored::ObjectFree), out),
        // `unset($v)` leaves `null`; unsetting an element or a property stores nothing.
        Node::Unset(u) => {
            for target in u.values.iter() {
                if let Expression::Variable(Variable::Direct(dv)) = target.unparenthesized() {
                    record(out, strip_dollar(bytes_to_string(dv.name)), Some(Stored::ObjectFree));
                }
            }
        }
        // `foreach ($it as &$v)` writes through its subject.
        Node::Foreach(fe) if fe.target.value().is_reference() => {
            store_into(fe.expression, None, out);
        }
        Node::ForeachValueTarget(t) => store_into(t.value, None, out),
        Node::ForeachKeyValueTarget(t) => {
            store_into(t.key, None, out);
            store_into(t.value, None, out);
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

/// Record that the assignment target `lhs` receives `value`. A variable stores
/// it; an element write `$a[…] = …` keeps `$a` an array and stores into it an
/// element holding `value`; a property write rebinds no variable; a
/// destructuring hands each target an element of `value`. Any other target
/// counts every variable in it as storing anything.
fn store_into(lhs: &Expression<'_>, value: Option<Stored>, out: &mut Stores) {
    match lhs.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            record(out, strip_dollar(bytes_to_string(dv.name)), value);
        }
        // The root stays an array (or a string, for an offset write into one),
        // and gains an element holding `value`. A chain through a property or
        // a call writes into no variable.
        Expression::ArrayAccess(_) | Expression::ArrayAppend(_) => {
            if let Some(root) = element_root(lhs) {
                let stored = value.filter(|s| *s == Stored::ObjectFree).unwrap_or(Stored::Array);
                record(out, root, Some(stored));
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
fn destructure(element: &ArrayElement<'_>, value: Option<Stored>, out: &mut Stores) {
    match element {
        ArrayElement::KeyValue(kv) => store_into(kv.value, value, out),
        ArrayElement::Value(v) => store_into(v.value, value, out),
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
fn store_into_root(expr: &Expression<'_>, out: &mut Stores) {
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
fn store_into_args(list: &ArgumentList<'_>, resolvable: bool, out: &mut Stores) {
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
