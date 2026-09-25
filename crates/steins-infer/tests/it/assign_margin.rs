//! The annotate margin's value fact for `$var = <rvalue>;` (ADR-0020): the
//! literal an assignment binds, shown on the assignment's line.
//!
//! Which arms of the assignment ladder show one is uneven. These fixtures pin
//! the arms that do beyond the folded literal `trace_annotation.rs` already
//! pins: an operator and a decided ternary. That the quiet arms (an offset
//! read, `??`, an array literal, `::class`, the shape rung, the envelope, the
//! declared floor) show nothing is where the ladder stands, not a ruling, so
//! nothing here pins it.

use steins_infer::{FactKind, NoFold, annotate_facts};
use steins_syntax::SourceTree;

/// The margin's rendering of `$var`'s value, if the margin shows one.
fn margin(src: &str, var: &str) -> Option<String> {
    let tree = SourceTree::parse(src);
    annotate_facts(&tree, &[], &[], "t.php", &mut NoFold).into_iter().find_map(|f| match f.kind {
        FactKind::Value { var: v, rendered } if v == var => Some(rendered),
        _ => None,
    })
}

#[test]
fn an_operator_rvalue_shows_its_literal() {
    assert_eq!(margin("<?php\n$b = 5 > 3;\n", "b").as_deref(), Some("true"));
}

#[test]
fn a_decided_ternary_shows_its_taken_arm() {
    assert_eq!(margin("<?php\n$t = true ? 1 : 2;\n", "t").as_deref(), Some("1"));
}
