//! The interpolated string's LOWERING (issue #627): `"a $v"` is the left-nested
//! `ArgValue::Concat` chain it desugars to, and a heredoc is deliberately not.

use steins_syntax::{ArgValue, SourceTree, StmtKind};

fn lowered(expr: &str) -> ArgValue {
    let src = format!("<?php\n$x = {expr};\n");
    let tree = SourceTree::parse(&src);
    tree.scopes()
        .iter()
        .flat_map(|sc| sc.stmts.iter())
        .find_map(|s| match &s.kind {
            StmtKind::Assign { var, value, .. } if var == "x" => Some(value.clone()),
            _ => None,
        })
        .expect("an assignment")
}

#[test]
fn an_interpolated_string_is_the_seeded_concat_chain() {
    // `"x $v"` is `Concat(Concat('', 'x '), $v)`: the seed, then the parts.
    match lowered("\"x $v\"") {
        ArgValue::Concat(head, tail) => {
            assert_eq!(*tail, ArgValue::Var("v".to_owned()));
            assert!(matches!(*head, ArgValue::Concat(..)), "the seed nests: {head:?}");
        }
        other => panic!("`\"x $v\"` lowered to {other:?}"),
    }
}

#[test]
fn a_lone_interpolated_variable_is_still_a_concatenation() {
    // `"$v"` is NOT `$v`: it is a concatenation, which is what makes it a string
    // cast. (The parser emits an empty literal part of its own here, so the head
    // is a nested chain of empty strings rather than one `Str`; what matters is
    // that the variable never stands alone.)
    match lowered("\"$v\"") {
        ArgValue::Concat(head, tail) => {
            assert_eq!(*tail, ArgValue::Var("v".to_owned()));
            assert!(!matches!(*head, ArgValue::Var(_)), "the seed vanished: {head:?}");
        }
        other => panic!("`\"$v\"` lowered to {other:?}"),
    }
}

#[test]
fn a_heredoc_still_widens_to_other() {
    // Its closing-marker indentation stripping is not in the parts.
    assert_eq!(lowered("<<<EOT\n  y $v\n  EOT"), ArgValue::Other);
}
