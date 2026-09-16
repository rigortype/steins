//! The value-position `isset` node's LOWERING (issue #579).
//!
//! `ArgValue::Isset` is **total**, which is the one property this file exists to
//! hold: an operand the vocabulary cannot spell becomes
//! `IssetOperand::Unmodelled`, and the expression never widens back to
//! `ArgValue::Other`. Widening is the defect the issue names — `isset` evaluates
//! to a `bool` whatever it tests, so `Other`'s `unknown` is not the safe side.
//!
//! It also pins the boundary against the GUARD lowering, which spells the same
//! two shapes under different rules: `empty(…)` stays out, and an offset key here
//! need not be the concrete literal `CondExpr::Isset` demands.

use steins_syntax::{ArgValue, IssetOperand, SourceTree, StmtKind};

/// The value assigned to `$var` by the first matching assignment.
fn assigned_value<'a>(tree: &'a SourceTree, var: &str) -> Option<&'a ArgValue> {
    tree.scopes().iter().find_map(|s| {
        s.stmts.iter().find_map(|st| match &st.kind {
            StmtKind::Assign { var: v, value, .. } if v == var => Some(value),
            _ => None,
        })
    })
}

fn lowered(expr: &str) -> ArgValue {
    let src = format!("<?php\n$x = {expr};\n");
    let tree = SourceTree::parse(&src);
    assigned_value(&tree, "x").expect("an assignment").clone()
}

/// The operands of a lowered `isset`, or a panic naming what arrived instead.
fn operands(expr: &str) -> Vec<IssetOperand> {
    match lowered(expr) {
        ArgValue::Isset(ops) => ops,
        other => panic!("`{expr}` lowered to {other:?}"),
    }
}

#[test]
fn a_bare_variable_operand_is_the_binding_question() {
    assert_eq!(operands("isset($a)"), vec![IssetOperand::Var("a".to_owned())]);
}

#[test]
fn a_depth_one_offset_operand_carries_its_base_and_key() {
    assert_eq!(
        operands("isset($a['k'])"),
        vec![IssetOperand::Offset {
            var: "a".to_owned(),
            key: Box::new(ArgValue::Str("k".into())),
        }]
    );
}

/// The offset key is whatever the value IR spells it as — NOT the concrete
/// literal `const_key_offset` requires of the guard form. A-G4 restricts the
/// guard because a tag discrimination is a claim about a written key; this
/// operand is resolved through the offset family's own key resolution, which
/// proves a variable key or declines it at the fact seam, not at lowering.
#[test]
fn a_variable_key_survives_lowering_where_the_guard_form_would_decline() {
    assert_eq!(
        operands("isset($a[$k])"),
        vec![IssetOperand::Offset {
            var: "a".to_owned(),
            key: Box::new(ArgValue::Var("k".to_owned())),
        }]
    );
}

/// Multi-argument `isset` is PHP's own conjunction, so every operand is carried
/// and the verdict is folded downstream — never a refusal at lowering.
#[test]
fn every_operand_of_a_multi_argument_isset_is_carried() {
    assert_eq!(
        operands("isset($a, $b['k'], $o->p)"),
        vec![
            IssetOperand::Var("a".to_owned()),
            IssetOperand::Offset {
                var: "b".to_owned(),
                key: Box::new(ArgValue::Str("k".into())),
            },
            IssetOperand::Unmodelled,
        ]
    );
}

/// The totality property itself: every shape this vocabulary does not spell
/// lands on `Unmodelled`, and NONE of them widens the expression to `Other`.
#[test]
fn an_unspellable_operand_is_unmodelled_never_a_widening() {
    for src in [
        "isset($o->p)",
        "isset(Foo::$p)",
        "isset($a['x']['y'])",
        "isset($$name)",
        "isset($o->p->q)",
        "isset(f()['k'])",
    ] {
        assert_eq!(operands(src), vec![IssetOperand::Unmodelled], "operands of `{src}`");
    }
}

/// `empty(…)` is a different question — `!isset(e) || !e`, whose second disjunct
/// is a truthiness reading of the operand's value. It keeps its `Other` lowering
/// until a slice asks it.
#[test]
fn empty_is_not_lowered_here() {
    assert_eq!(lowered("empty($a)"), ArgValue::Other);
    assert_eq!(lowered("empty($a['k'])"), ArgValue::Other);
}

/// The node reaches a call argument by the same lowering, which is what lets the
/// debug surface and `f(isset($x))` answer without a second code path.
#[test]
fn a_call_argument_lowers_to_the_same_node() {
    let tree = SourceTree::parse("<?php\nf(isset($a['k']));\n");
    let call = tree
        .scopes()
        .iter()
        .find_map(|s| {
            s.stmts.iter().find_map(|st| match &st.kind {
                StmtKind::Call(c) => Some(c),
                _ => None,
            })
        })
        .expect("a statement call");
    match &call.args[0].value {
        ArgValue::Isset(ops) => assert_eq!(ops.len(), 1),
        other => panic!("argument lowered to {other:?}"),
    }
}

/// The rendering a diagnostic message would show, so a future consumer does not
/// print `<expr>` for a node that knows its own spelling.
#[test]
fn the_node_renders_as_the_construct() {
    assert_eq!(lowered("isset($a)").render(), "isset($a)");
    assert_eq!(lowered("isset($a['k'], $o->p)").render(), "isset($a[\"k\"], <expr>)");
}
