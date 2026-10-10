//! Which expression statements are no-ops (issue #946, walker coverage run #1033 S4).
//!
//! A statement the lowering does not recognise is a [`StmtKind::Barrier`], and the walk
//! forgets every local at a barrier. For `1;` and the literal arms of a `match`
//! (`1 => 10`, which lower through the same path) that threw away a scope's whole
//! environment for a statement that cannot change it.
//!
//! The bar is the one ADR-0096 sets for a dead statement: a no-op **can neither raise a
//! diagnostic nor throw**. A warning, notice or deprecation reaches `set_error_handler`,
//! which is user code that may write any local (`~1.5;` raises a deprecation, `'ab'[5];`
//! a warning), and a throw leaves the scope, so the code after it is not reached with
//! the values before it. The set is therefore small and closed; everything else stays a
//! barrier. Each refusal below is witnessed on PHP 8.5.11 with a handler that rebinds a
//! local.
//!
//! | expression                                              | no-op? | reason |
//! | ------------------------------------------------------- | ------ | ------ |
//! | literal (int, float, string, `true`, `false`, `null`), magic constant | yes | nothing to evaluate |
//! | `$x;` as a whole statement (not `$this`)                | yes    | compiled away: no read, so no undefined-variable warning |
//! | `isset($x)`, `empty($x)` over bare variables            | yes    | never warns, never calls `offsetExists` without a fetch |
//! | `[…]` / `array(…)` of allowed values; keys, if any, are distinct int or string literals | yes | no `Illegal offset` and no overwrite to reason about |
//! | `!a`, `a === b`, `a !== b` over allowed values with no variable | yes | identity and negation convert nothing |
//! | `$x` anywhere inside a larger expression                | no     | an undefined variable warns: `$zz ? 1 : 2;`, `-$zz;`, `(int)$zz;`, `$zz instanceof Foo;` |
//! | `$x;` as a match arm                                    | no     | the arm's value is consumed, so the read warns |
//! | `~1.5`, `1.5 \| 1`, `(int) 1e30`, `0 ** -1`, `'1a' + 1`  | no     | a deprecation or warning |
//! | `['a'=>1]['b']`, `'ab'[5]`, `[1,2][1.5]`, `[1][null]`    | no     | a warning or deprecation on the fetch |
//! | `[1.5 => 1]`, `[null => 1]`, `[true => 1]`, a repeated key | no   | a deprecation, or a key collision |
//! | `1/0`, `1 % 0`, `-[1]`, `1 << -1`, any arithmetic over an operand that is not a number | no | throws `DivisionByZeroError`, `TypeError`, `ArithmeticError` |
//! | `.`, loose comparison, casts, `??`, `?:`, `instanceof`  | no     | `__toString`, a warning, or a nested read; arithmetic and bitwise operators share the row above |
//! | a fetch of a property, array element, static member or constant | no | `__get`, `offsetGet`, autoload, an undefined constant |
//! | calls, `new`, `include`, `eval`, `print`, `echo`, assignments, `++`/`--`, `@`, `clone`, `yield`, `throw`, `exit`, `match`, closures | no | run code, write, print or terminate |
//!
//! [`StmtKind::Barrier`]: crate::ast::StmtKind::Barrier

use mago_syntax::cst::{
    ArrayElement, BinaryOperator, Construct, Expression, Literal, UnaryPrefixOperator, Variable,
};

use crate::bytes_to_string;

/// Whether `expr`, in a position whose value is consumed (a match arm), is a no-op.
pub(crate) fn is_pure_expr(expr: &Expression<'_>) -> bool {
    is_value(expr)
        || match expr.unparenthesized() {
            Expression::Construct(Construct::Isset(i)) => i.values.iter().all(|v| is_plain_read(v)),
            Expression::Construct(Construct::Empty(e)) => is_plain_read(e.value),
            _ => false,
        }
}

/// Whether `expr`, as a whole statement, is a no-op: [`is_pure_expr`], plus a bare local
/// read, which PHP compiles away.
pub(crate) fn is_pure_statement(expr: &Expression<'_>) -> bool {
    is_pure_expr(expr) || is_plain_read(expr)
}

/// A direct variable that is not `$this`.
fn is_plain_read(e: &Expression<'_>) -> bool {
    matches!(
        e.unparenthesized(),
        Expression::Variable(Variable::Direct(d)) if bytes_to_string(d.name) != "$this"
    )
}

/// A value that evaluates without reading a variable, warning or throwing.
fn is_value(e: &Expression<'_>) -> bool {
    match e {
        Expression::Parenthesized(p) => is_value(p.expression),
        Expression::Literal(_) | Expression::MagicConstant(_) => true,
        Expression::UnaryPrefix(u) => {
            matches!(u.operator, UnaryPrefixOperator::Not(_)) && is_value(u.operand)
        }
        Expression::Binary(b) => {
            matches!(b.operator, BinaryOperator::Identical(_) | BinaryOperator::NotIdentical(_))
                && is_value(b.lhs)
                && is_value(b.rhs)
        }
        Expression::Array(a) => is_array_literal(a.elements.iter()),
        Expression::LegacyArray(a) => is_array_literal(a.elements.iter()),
        _ => false,
    }
}

/// Values only, or key/value pairs only (a mix lets an automatic key meet a literal
/// one), whose keys are distinct decimal int or non-numeric string literals.
fn is_array_literal<'a, 'arena: 'a>(
    elements: impl Iterator<Item = &'a ArrayElement<'arena>>,
) -> bool {
    let mut keys: Vec<&[u8]> = Vec::new();
    let (mut plain, mut keyed) = (false, false);
    for e in elements {
        match e {
            ArrayElement::Value(v) => {
                plain = true;
                if !is_value(v.value) {
                    return false;
                }
            }
            ArrayElement::KeyValue(kv) => {
                keyed = true;
                let key: &[u8] = match kv.key.unparenthesized() {
                    // `'1'` is the key `1`, and `"a"` is `'a'`: decoded, and not numeric.
                    Expression::Literal(Literal::String(s)) => match s.value {
                        Some(v) if !v.iter().any(u8::is_ascii_digit) => v,
                        _ => return false,
                    },
                    // `0x1`, `1_0` and `01` spell keys another way.
                    Expression::Literal(Literal::Integer(i))
                        if i.raw.iter().all(u8::is_ascii_digit)
                            && (i.raw == b"0" || i.raw.first() != Some(&b'0')) =>
                    {
                        i.raw
                    }
                    _ => return false,
                };
                if keys.contains(&key) || !is_value(kv.value) {
                    return false;
                }
                keys.push(key);
            }
            ArrayElement::Variadic(_) | ArrayElement::Missing(_) => return false,
        }
    }
    !(plain && keyed)
}
