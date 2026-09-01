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
    // The node carries only operators SOME consumer answers (Certainty
    // discipline). `|` is the one non-comparison member (issue #615) and has its
    // own row below.
    for src in ["1 + 1", "1 - 1", "2 * 3", "7 / 2", "7 % 2", "2 ** 3", "5 & 3", "5 ^ 3",
                "1 << 2", "8 >> 1", "true && false", "true || false", "1 <=> 2"] {
        assert_eq!(lowered(src), ArgValue::Other, "`{src}` must not lower to the node yet");
    }
    // `.` keeps its own dedicated variant (issue #59), and `??` keeps `Coalesce`.
    assert!(matches!(lowered("'a' . 'b'"), ArgValue::Concat(..)));
    assert!(matches!(lowered("$a ?? 1"), ArgValue::Coalesce(..)));
}

#[test]
fn a_bitwise_or_lowers_to_the_node_and_answers_nothing_of_its_own() {
    // Issue #615: `|` joined the node for the `filter_var` flags roster, which
    // resolves flag CONSTANTS by name. It reaches no fact seam — a bitwise `|`
    // has no total floor, since GMP overloads it to return an object — so the
    // node is carried structurally and stays unproven, exactly like a comparison
    // nobody decided.
    match lowered("5 | 3") {
        ArgValue::Binary { op: ValueOp::BitOr, lhs, rhs } => {
            assert_eq!(*lhs, ArgValue::Int(5));
            assert_eq!(*rhs, ArgValue::Int(3));
        }
        other => panic!("lowered to {other:?}"),
    }
    // Left-nested, matching PHP associativity, so a three-term flag chain is two
    // nodes rather than a flat list.
    match lowered("FILTER_A | FILTER_B | FILTER_C") {
        ArgValue::Binary { op: ValueOp::BitOr, lhs, rhs } => {
            assert!(matches!(*lhs, ArgValue::Binary { op: ValueOp::BitOr, .. }));
            assert!(matches!(*rhs, ArgValue::GlobalConst(_)));
        }
        other => panic!("lowered to {other:?}"),
    }
    let v = lowered("5 | 3");
    assert!(!v.is_literal(), "a `|` is never a proven value");
    assert!(!v.is_concrete_value());
    assert_eq!(ValueOp::BitOr.symbol(), "|");
    // The reason it is carried at all: an `Other` ELEMENT collapses its whole
    // enclosing array literal, so with `|` unrepresented `['flags' => A | B]`
    // was not an array and no rule could read even its key.
    match lowered("['flags' => FILTER_A | FILTER_B]") {
        ArgValue::Array(items) => {
            assert_eq!(items.len(), 1);
            assert!(matches!(items[0].1, ArgValue::Binary { op: ValueOp::BitOr, .. }));
        }
        other => panic!("lowered to {other:?}"),
    }
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
