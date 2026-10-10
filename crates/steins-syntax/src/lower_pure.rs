//! Which expression statements write nothing (issue #946, walker coverage run #1033 S4).
//!
//! A statement the lowering does not recognise is a [`StmtKind::Barrier`], and the walk
//! forgets every local at a barrier. For `1;`, `$i;` and the literal arms of a `match`
//! (`1 => 10`, which lower through the same path) that threw away a scope's whole
//! environment for a statement that cannot change it. [`is_pure_expr`] names the
//! expressions that are a no-op for the walk: no call, no write, no output, no change of
//! `error_reporting`, and no user code run behind the engine's back. Every entry is
//! witnessed on PHP 8.5.11.
//!
//! | expression                                            | pure? | reason |
//! | ----------------------------------------------------- | ----- | ------ |
//! | literal, magic constant (`__LINE__`, `__CLASS__`, …)  | yes   | nothing to run |
//! | `$x` (a direct variable other than `$this`)           | yes   | a read; `$this` outside an object throws |
//! | `(e)`, `e ? a : b`, `e ?: b`, `a ?? b`, `a && b`, `a \|\| b`, `a xor b`, `!a` | if every part is | short-circuit and boolean conversion run no user code |
//! | `a + b`, `-`, `*`, `/`, `%`, `**`, `<<`, `>>`, `&`, `\|`, `^`, `~a`, `-a`, `+a` | if both parts are | an object operand is a `TypeError`, never a user method; `1/0` and a bad operand throw an engine `Error`, which leaves the scope as an uncaught call's would |
//! | `a === b`, `a !== b`                                  | if both parts are | identity, no conversion |
//! | `a . b`, `(string) a`, `(binary) a`                   | if every operand is object-free | `__toString` runs for an object |
//! | `a == b`, `!=`, `<>`, `<`, `<=`, `>`, `>=`, `<=>`     | if both parts are, and object-free | `$o == 'x'` and `$o < 'y'` call `__toString` |
//! | `(int)`, `(float)`, `(bool)` cast (and aliases)       | if the operand is | no user code, even for an object |
//! | `(array) a`                                           | if the operand is object-free | a lazy object initialises |
//! | `(object) a`                                          | if the operand is | builds a `stdClass` |
//! | `a instanceof Name`                                   | if `a` is | no autoload (witnessed) |
//! | `[…]`, `array(…)` with values and keys                | if every element is | spread (`...$x`) iterates a `Traversable`, `&$x` aliases |
//! | `[…][k]`, `'abc'[k]` (a literal receiver)             | if `k` and the elements are | a real array, never an `ArrayAccess` |
//! | `isset($x)`, `empty($x)` (direct variables)          | yes   | no `offsetExists` without a fetch |
//! | `$a[k]`, `$o->p`, `A::$p`, `A::C`, `A::case`, `$o?->p` | no  | `offsetGet`, `__get`, autoload (witnessed) |
//! | `FOO` (any constant, `true`/`false`/`null` aside)     | no    | an undefined constant throws; whether it is defined is not known here |
//! | `f()`, `$o->m()`, `A::m()`, `new`, `include`, `eval`, `print`, `` `cmd` `` | no | runs code |
//! | `echo`, `print`                                       | no    | output |
//! | `$x = …`, `+=`, `$x++`, `--$x`, `&$x`                 | no    | a write |
//! | `@e`                                                  | no    | changes `error_reporting` |
//! | `clone`, `yield`, `throw`, `exit`, `match`            | no    | `__clone`, suspension, a terminator, arms with their own structure |
//! | closures, arrow functions, `new class`                | no    | a by-reference `use` aliases; a class declaration |
//!
//! "Object-free" is a property of the expression, since the lowering has no types: it
//! can never evaluate to an object, nor to an array that holds one. That is a literal,
//! a magic constant, the result of an operator that produces a scalar, and an array
//! literal or `+` of object-free parts (or with a scalar literal). A variable is not object-free, so `$s . 'a'` and
//! `$s == 'a'` stay barriers: the lowering cannot tell a string from a `Stringable`.
//!
//! [`StmtKind::Barrier`]: crate::ast::StmtKind::Barrier

use mago_syntax::cst::{
    ArrayElement, BinaryOperator, Construct, Expression, Literal, UnaryPrefixOperator, Variable,
};

use crate::bytes_to_string;

/// Whether evaluating `expr` as a statement is a no-op for the walk (the table above).
pub(crate) fn is_pure_expr(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::Parenthesized(p) => is_pure_expr(p.expression),
        Expression::Literal(_) | Expression::MagicConstant(_) => true,
        Expression::Variable(v) => is_plain_variable(v),
        Expression::UnaryPrefix(u) => is_pure_prefix(&u.operator, u.operand),
        Expression::Binary(b) => is_pure_binary(&b.operator, b.lhs, b.rhs),
        Expression::Conditional(c) => {
            is_pure_expr(c.condition) && c.then.is_none_or(is_pure_expr) && is_pure_expr(c.r#else)
        }
        Expression::Array(a) => a.elements.iter().all(is_pure_element),
        Expression::LegacyArray(a) => a.elements.iter().all(is_pure_element),
        Expression::ArrayAccess(a) => {
            is_literal_receiver(a.array) && is_pure_expr(a.array) && is_pure_expr(a.index)
        }
        Expression::Construct(Construct::Isset(i)) => i.values.iter().all(|v| is_plain_read(v)),
        Expression::Construct(Construct::Empty(e)) => is_plain_read(e.value),
        _ => false,
    }
}

/// A direct variable that is not `$this`.
fn is_plain_variable(v: &Variable<'_>) -> bool {
    matches!(v, Variable::Direct(d) if bytes_to_string(d.name) != "$this")
}

/// A bare local read: `isset($x)` and `empty($x)` reach `offsetExists` and `__isset` only
/// through a fetch.
fn is_plain_read(e: &Expression<'_>) -> bool {
    matches!(e.unparenthesized(), Expression::Variable(v) if is_plain_variable(v))
}

/// An array literal's element: a value or a key/value pair of pure parts.
fn is_pure_element(e: &ArrayElement<'_>) -> bool {
    match e {
        ArrayElement::Value(v) => is_pure_expr(v.value),
        ArrayElement::KeyValue(kv) => is_pure_expr(kv.key) && is_pure_expr(kv.value),
        ArrayElement::Variadic(_) | ArrayElement::Missing(_) => false,
    }
}

/// An array literal or a string literal: a fetch on one never reaches `ArrayAccess`.
fn is_literal_receiver(e: &Expression<'_>) -> bool {
    matches!(
        e.unparenthesized(),
        Expression::Array(_) | Expression::LegacyArray(_) | Expression::Literal(Literal::String(_))
    )
}

fn is_pure_prefix(op: &UnaryPrefixOperator<'_>, operand: &Expression<'_>) -> bool {
    use UnaryPrefixOperator as U;
    match op {
        U::Not(_)
        | U::BitwiseNot(_)
        | U::Plus(_)
        | U::Negation(_)
        | U::BoolCast(..)
        | U::BooleanCast(..)
        | U::DoubleCast(..)
        | U::RealCast(..)
        | U::FloatCast(..)
        | U::IntCast(..)
        | U::IntegerCast(..)
        | U::ObjectCast(..) => is_pure_expr(operand),
        U::StringCast(..) | U::BinaryCast(..) | U::ArrayCast(..) => {
            is_pure_expr(operand) && is_object_free(operand)
        }
        U::ErrorControl(_)
        | U::Reference(_)
        | U::UnsetCast(..)
        | U::VoidCast(..)
        | U::PreIncrement(_)
        | U::PreDecrement(_) => false,
    }
}

fn is_pure_binary(op: &BinaryOperator<'_>, lhs: &Expression<'_>, rhs: &Expression<'_>) -> bool {
    use BinaryOperator as B;
    match op {
        // `instanceof Name` loads no class, so a plain name on the right is the whole
        // form; `instanceof $cls` and `instanceof (expr)` stay barriers.
        B::Instanceof(_) => {
            is_pure_expr(lhs) && matches!(rhs.unparenthesized(), Expression::Identifier(_))
        }
        // Converts an operand to a string, or compares loosely: a `__toString`.
        B::StringConcat(_)
        | B::Equal(_)
        | B::NotEqual(_)
        | B::AngledNotEqual(_)
        | B::LessThan(_)
        | B::LessThanOrEqual(_)
        | B::GreaterThan(_)
        | B::GreaterThanOrEqual(_)
        | B::Spaceship(_) => {
            is_pure_expr(lhs) && is_pure_expr(rhs) && is_object_free(lhs) && is_object_free(rhs)
        }
        _ => is_pure_expr(lhs) && is_pure_expr(rhs),
    }
}

/// Whether a PURE expression can never evaluate to an object, nor to an array holding
/// one: the operands `.` and a loose comparison may convert without running a
/// `__toString`. Asked only of an expression [`is_pure_expr`] accepted.
fn is_object_free(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::Parenthesized(p) => is_object_free(p.expression),
        Expression::Literal(_) | Expression::MagicConstant(_) => true,
        Expression::Binary(b) => match b.operator {
            // Passes an operand through.
            BinaryOperator::NullCoalesce(_) => is_object_free(b.lhs) && is_object_free(b.rhs),
            // Adds arrays together, unless an operand is a scalar literal, which makes
            // it a number or a `TypeError`.
            BinaryOperator::Addition(_) => {
                is_scalar_literal(b.lhs)
                    || is_scalar_literal(b.rhs)
                    || (is_object_free(b.lhs) && is_object_free(b.rhs))
            }
            // Every other binary operator produces a scalar.
            _ => true,
        },
        Expression::UnaryPrefix(u) => match u.operator {
            UnaryPrefixOperator::ObjectCast(..) => false,
            UnaryPrefixOperator::ArrayCast(..) => is_object_free(u.operand),
            _ => true,
        },
        // `a ?: b` yields `a` itself when it is truthy.
        Expression::Conditional(c) => {
            is_object_free(c.then.unwrap_or(c.condition)) && is_object_free(c.r#else)
        }
        Expression::Array(a) => a.elements.iter().all(is_object_free_element),
        Expression::LegacyArray(a) => a.elements.iter().all(is_object_free_element),
        Expression::ArrayAccess(a) => is_object_free(a.array),
        Expression::Construct(_) => true,
        _ => false,
    }
}

fn is_scalar_literal(expr: &Expression<'_>) -> bool {
    matches!(expr.unparenthesized(), Expression::Literal(_))
}

fn is_object_free_element(e: &ArrayElement<'_>) -> bool {
    match e {
        ArrayElement::Value(v) => is_object_free(v.value),
        ArrayElement::KeyValue(kv) => is_object_free(kv.value),
        ArrayElement::Variadic(_) | ArrayElement::Missing(_) => false,
    }
}
