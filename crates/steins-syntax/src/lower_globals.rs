//! `$GLOBALS[<key>]` as a write channel (issue #902, walker coverage run #1033 D5).
//!
//! At file scope `$GLOBALS['x']` IS the local `$x`, so a write through it rebinds a name
//! the source never spells as a variable. Every collector that asks "what may this
//! subtree write" asks here, so the answer is one rule table rather than a clause per
//! collector. Witnessed on PHP 8.5.11:
//!
//! | write form (lvalue spine rooted at the fetch)            | literal key `'x'`   | dynamic key `$k`        |
//! | -------------------------------------------------------- | ------------------- | ----------------------- |
//! | `$GLOBALS[k] = v`, `+=`, `??=`                           | write of `x`        | poisons (`$$`'s family) |
//! | `$GLOBALS[k][] = v`, `[j] = v`, `->p = v`                | write of `x`        | poisons                 |
//! | `++$GLOBALS[k]`, `$GLOBALS[k]--`                         | write of `x`        | poisons                 |
//! | `unset($GLOBALS[k])`                                     | write of `x`        | poisons                 |
//! | `[$GLOBALS[k], $y] = …`, `list(…)`, `['a' => $GLOBALS[k]] = …` | write of `x`  | poisons                 |
//! | `foreach ($it as $GLOBALS[k])`, `… as $k2 => $GLOBALS[k]`| write of `x`        | poisons                 |
//! | `$r = &$GLOBALS[k]`, `&$GLOBALS[k]` anywhere             | `ReferenceAssign` / `ReferenceBinding` poison (as for any `&`), and a write of `x` | same |
//! | `f($GLOBALS[k])`, `f($GLOBALS[k][j])` (by-ref candidate) | `x` handed to the call (the callee's signature decides) | poisons |
//! | a read: `$GLOBALS[k]`, `isset`, `empty`, `??`            | nothing             | nothing                 |
//! | `$GLOBALS = …`, `$GLOBALS += …`                          | a compile-time fatal since 8.1: no form to model | — |
//!
//! The scope axis is flat. A literal key is a write of that name in EVERY scope: in a
//! function `$GLOBALS['x']` is the global `$x`, not the local, so the walk forgets a
//! same-named local it need not forget, which only costs precision. A dynamic key
//! poisons in every scope as `extract` and `$$v` do, recorded as
//! [`OpaqueConstruct::VariableVariable`] — a variable named at run time. Reading the
//! do-while header afterwards is a separate follow-up.
//!
//! [`OpaqueConstruct::VariableVariable`]: crate::ast::OpaqueConstruct::VariableVariable

use mago_syntax::cst::{
    Access, Argument, ArgumentList, ArrayElement, Expression, Node, UnaryPrefixOperator, Variable,
};

use crate::lower_expr::lower_arg_value;
use crate::ast::ArgValue;
use crate::stack_guard;
use crate::{bytes_to_string, strip_dollar};

/// The key of one `$GLOBALS[<key>]` fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GlobalsKey {
    /// A string or integer literal: the name of the global it aliases.
    Literal(String),
    /// Anything else, including a string with no UTF-8 spelling: a name chosen at run
    /// time.
    Dynamic,
}

/// `$GLOBALS[<key>]` itself (one fetch, not a chain), or `None` for any other expression.
pub(crate) fn globals_fetch(expr: &Expression<'_>) -> Option<GlobalsKey> {
    let Expression::ArrayAccess(aa) = expr.unparenthesized() else { return None };
    let Expression::Variable(Variable::Direct(dv)) = aa.array.unparenthesized() else {
        return None;
    };
    if strip_dollar(bytes_to_string(dv.name)) != "GLOBALS" {
        return None;
    }
    Some(match lower_arg_value(aa.index) {
        ArgValue::Str(s) => s.as_str().map_or(GlobalsKey::Dynamic, |s| GlobalsKey::Literal(s.to_owned())),
        ArgValue::Int(i) => GlobalsKey::Literal(i.to_string()),
        _ => GlobalsKey::Dynamic,
    })
}

/// Every `$GLOBALS[<key>]` an **lvalue** writes through, found along its spine: the
/// offset chain (`$GLOBALS['x']['k'][]`), the property object (`$GLOBALS['x']->p`) and
/// every target of a destructuring pattern. An index or a destructuring key is a read
/// and is not walked. Mirrors `bind_lvalue_roots`, which binds the root LOCAL of the
/// same spine.
pub(crate) fn lvalue_globals(expr: &Expression<'_>, visit: &mut dyn FnMut(GlobalsKey)) {
    // `$a[0][0][…] = …` walks one frame per subscript (issue #264).
    if stack_guard::exhausted() {
        return;
    }
    let expr = expr.unparenthesized();
    match expr {
        Expression::ArrayAccess(aa) => match globals_fetch(expr) {
            Some(key) => visit(key),
            None => lvalue_globals(aa.array, visit),
        },
        Expression::ArrayAppend(ap) => lvalue_globals(ap.array, visit),
        Expression::Access(Access::Property(pa)) => lvalue_globals(pa.object, visit),
        Expression::Access(Access::NullSafeProperty(pa)) => lvalue_globals(pa.object, visit),
        Expression::Array(a) => destructured_globals(a.elements.iter(), visit),
        Expression::LegacyArray(a) => destructured_globals(a.elements.iter(), visit),
        Expression::List(l) => destructured_globals(l.elements.iter(), visit),
        Expression::UnaryPrefix(up) if matches!(up.operator, UnaryPrefixOperator::Reference(_)) => {
            lvalue_globals(up.operand, visit);
        }
        _ => {}
    }
}

fn destructured_globals<'a>(
    elements: impl Iterator<Item = &'a ArrayElement<'a>>,
    visit: &mut dyn FnMut(GlobalsKey),
) {
    for element in elements {
        match element {
            ArrayElement::KeyValue(kv) => lvalue_globals(kv.value, visit),
            ArrayElement::Value(v) => lvalue_globals(v.value, visit),
            ArrayElement::Variadic(v) => lvalue_globals(v.value, visit),
            ArrayElement::Missing(_) => {}
        }
    }
}

/// Every `$GLOBALS[<key>]` the node ITSELF writes (its descendants are the caller's
/// recursion): an assignment's left side, an increment or decrement, an `unset` target
/// and a `foreach` target. A call argument is [`argument_globals`]'s question.
pub(crate) fn node_globals_writes(node: &Node<'_, '_>, visit: &mut dyn FnMut(GlobalsKey)) {
    match node {
        Node::Assignment(a) => {
            lvalue_globals(a.lhs, visit);
            // `$r = &$GLOBALS['x']` ties `$r` to the global (also a `ReferenceAssign` poison).
            if a.rhs.is_reference() {
                lvalue_globals(a.rhs, visit);
            }
        }
        Node::UnaryPrefix(u)
            if matches!(
                u.operator,
                UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_)
            ) =>
        {
            lvalue_globals(u.operand, visit);
        }
        Node::UnaryPostfix(u) => lvalue_globals(u.operand, visit),
        Node::Unset(u) => {
            for target in u.values.iter() {
                lvalue_globals(target, visit);
            }
        }
        Node::ForeachValueTarget(t) => lvalue_globals(t.value, visit),
        Node::ForeachKeyValueTarget(t) => {
            lvalue_globals(t.key, visit);
            lvalue_globals(t.value, visit);
        }
        _ => {}
    }
}

/// Append the literal names the node itself writes through `$GLOBALS`, first-seen order.
pub(crate) fn push_globals_writes(node: &Node<'_, '_>, out: &mut Vec<String>) {
    node_globals_writes(node, &mut |key| {
        if let GlobalsKey::Literal(name) = key
            && !out.contains(&name)
        {
            out.push(name);
        }
    });
}

/// Append the literal names an **lvalue expression** writes through `$GLOBALS`.
pub(crate) fn push_lvalue_globals(expr: &Expression<'_>, out: &mut Vec<String>) {
    lvalue_globals(expr, &mut |key| {
        if let GlobalsKey::Literal(name) = key
            && !out.contains(&name)
        {
            out.push(name);
        }
    });
}

/// Whether the node itself writes through a `$GLOBALS` fetch whose key is not a
/// literal.
pub(crate) fn node_writes_dynamic_globals(node: &Node<'_, '_>) -> bool {
    let mut dynamic = false;
    node_globals_writes(node, &mut |key| dynamic |= key == GlobalsKey::Dynamic);
    dynamic
}

/// The `$GLOBALS[<key>]` a call ARGUMENT hands over, when the argument is a pure offset
/// chain rooted at the fetch (`$GLOBALS['x']`, `$GLOBALS['x'][0]`): a by-ref parameter
/// there writes through into global `x`.
pub(crate) fn argument_globals(arg: &Expression<'_>) -> Option<GlobalsKey> {
    let mut e = arg.unparenthesized();
    loop {
        if let Some(key) = globals_fetch(e) {
            return Some(key);
        }
        match e {
            Expression::ArrayAccess(aa) => e = aa.array.unparenthesized(),
            _ => return None,
        }
    }
}

/// The argument list of a call node, whatever its spelling.
pub(crate) fn call_arguments<'a, 'arena>(node: &'a Node<'a, 'arena>) -> Option<&'a ArgumentList<'arena>> {
    match node {
        Node::FunctionCall(c) => Some(&c.argument_list),
        Node::MethodCall(c) => Some(&c.argument_list),
        Node::NullSafeMethodCall(c) => Some(&c.argument_list),
        Node::StaticMethodCall(c) => Some(&c.argument_list),
        Node::Instantiation(i) => i.argument_list.as_ref(),
        _ => None,
    }
}

/// Whether the call node hands a dynamic-key `$GLOBALS` fetch to its callee.
pub(crate) fn call_hands_dynamic_globals(node: &Node<'_, '_>) -> bool {
    call_arguments(node).is_some_and(|list| {
        list.arguments.iter().any(|a: &Argument<'_>| {
            argument_globals(a.value()) == Some(GlobalsKey::Dynamic)
        })
    })
}
