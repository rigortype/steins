//! The operator-value node's LOWERING (issue #260).
//!
//! `ArgValue::Binary` is structural, like `Concat`/`Coalesce`: operands stay
//! unevaluated since their values are env facts the walk owns. This file pins
//! the boundary — which operators the node carries, and that everything else
//! still widens to `Other`.

use steins_syntax::{ArgValue, CmpOp, SourceTree, StmtKind, ValueOp};

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

#[test]
fn every_comparison_operator_lowers_to_the_node() {
    for (src, op) in [
        ("1 === 2", CmpOp::Identical),
        ("1 !== 2", CmpOp::NotIdentical),
        ("1 == 2", CmpOp::Loose),
        ("1 != 2", CmpOp::NotLoose),
        ("1 <> 2", CmpOp::NotLoose),
        ("1 < 2", CmpOp::Lt),
        ("1 <= 2", CmpOp::Le),
        ("1 > 2", CmpOp::Gt),
        ("1 >= 2", CmpOp::Ge),
    ] {
        match lowered(src) {
            ArgValue::Binary { op: ValueOp::Cmp(got), lhs, rhs } => {
                assert_eq!(got, op, "operator of `{src}`");
                assert_eq!(*lhs, ArgValue::Int(1));
                assert_eq!(*rhs, ArgValue::Int(2));
            }
            other => panic!("`{src}` lowered to {other:?}"),
        }
    }
}

#[test]
fn operands_keep_their_own_lowering() {
    // Operands are whatever the value IR spells them as — a `Var` here, whose
    // value only the walk knows; that is the whole reason the node is structural.
    match lowered("$a === 'x'") {
        ArgValue::Binary { lhs, rhs, .. } => {
            assert_eq!(*lhs, ArgValue::Var("a".to_owned()));
            assert_eq!(*rhs, ArgValue::Str("x".into()));
        }
        other => panic!("lowered to {other:?}"),
    }
}

#[test]
fn an_unrepresentable_operand_stays_in_the_tree() {
    // Unlike an array literal, an unspellable operand does NOT collapse the node:
    // the operator still guarantees a `bool`, so the walk declines on the value alone.
    match lowered("$a->b->c === 1") {
        ArgValue::Binary { lhs, .. } => assert_eq!(*lhs, ArgValue::Other),
        other => panic!("lowered to {other:?}"),
    }
}

#[test]
fn non_comparison_operators_still_widen() {
    // The node carries only operators an evaluator answers (Certainty discipline).
    for src in ["1 + 1", "1 - 1", "2 * 3", "7 / 2", "7 % 2", "2 ** 3", "5 & 3", "5 | 3", "5 ^ 3",
                "1 << 2", "8 >> 1", "true && false", "true || false", "1 <=> 2"] {
        assert_eq!(lowered(src), ArgValue::Other, "`{src}` must not lower to the node yet");
    }
    // `.` keeps its own dedicated variant (issue #59), and `??` keeps `Coalesce`.
    assert!(matches!(lowered("'a' . 'b'"), ArgValue::Concat(..)));
    assert!(matches!(lowered("$a ?? 1"), ArgValue::Coalesce(..)));
}

#[test]
fn the_node_is_not_a_proven_value() {
    // `is_literal`/`is_concrete_value` gate every proof-layer consumer; a
    // comparison becomes a value only by being decided, never by being written.
    let v = lowered("1 === 1");
    assert!(!v.is_literal());
    assert!(!v.is_concrete_value());
}

#[test]
fn the_node_renders_as_written() {
    assert_eq!(lowered("$a === 1").render(), "($a === 1)");
    assert_eq!(lowered("1 <= 2").render(), "(1 <= 2)");
}
