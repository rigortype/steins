//! The value-position cast node's LOWERING (issue #626).
//!
//! `ArgValue::Cast` is structural, like `Not`/`Binary`: the operand stays
//! unevaluated, since its value is an env fact the walk owns. This file pins the
//! boundary — which cast tokens carry a target, that the node is **total** over
//! its operand, and that the four tokens with no statable conversion still widen
//! to `Other`.
//!
//! # Behavioral witnesses at `PINNED_PHP` (8.5.9, `php -r`)
//!
//! The token set here is NOT `settype`'s type-string set, and the difference is
//! measured in both directions:
//!
//! * `var_dump((binary)"a");` — `string(1) "a"`, with `Deprecated: Non-canonical
//!   cast (binary) is deprecated, use the (string) cast instead`. A working
//!   string cast, where `settype($v, 'binary')` is a `ValueError`.
//! * `var_dump((integer)"5", (double)"5", (boolean)1);` — `int(5)`, `float(5)`,
//!   `bool(true)`, each with the same non-canonical deprecation. They convert
//!   identically to their canonical spellings.
//! * `var_dump((object)1);` — `object(stdClass)#1 (1) { ["scalar"]=> int(1) }`.
//! * `var_dump((real)1);` — `Parse error: The (real) cast has been removed, use
//!   (float) instead`.
//! * `var_dump((unset)1);` — `Fatal error: The (unset) cast is no longer
//!   supported`.
//! * `var_dump((void)1);` — `Parse error: syntax error, unexpected token
//!   "(void)"`. PHP has no such cast; mago lexes the token anyway.
//! * `var_dump((null)1);` — `Parse error: syntax error, unexpected integer "1",
//!   expecting ")"`. There is no `(null)` cast, which is why `CastTarget::Null`
//!   is reachable from `settype` alone.

use steins_syntax::{ArgValue, CastTarget, SourceTree, StmtKind};

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
fn every_converting_cast_token_lowers_to_the_node() {
    for (src, want) in [
        ("(int) $a", CastTarget::Int),
        ("(integer) $a", CastTarget::Int),
        ("(float) $a", CastTarget::Float),
        ("(double) $a", CastTarget::Float),
        ("(string) $a", CastTarget::String),
        ("(binary) $a", CastTarget::String),
        ("(bool) $a", CastTarget::Bool),
        ("(boolean) $a", CastTarget::Bool),
        ("(array) $a", CastTarget::Array),
    ] {
        match lowered(src) {
            ArgValue::Cast { target, operand } => {
                assert_eq!(target, want, "target of `{src}`");
                assert_eq!(*operand, ArgValue::Var("a".to_owned()), "operand of `{src}`");
            }
            other => panic!("`{src}` lowered to {other:?}"),
        }
    }
}

#[test]
fn the_casts_php_refuses_widen_to_other() {
    // `(object)` converts but writes a `stdClass` the value domain has no member
    // for; the other three cannot run at all (see the module witnesses), so
    // there is no behaviour to state for any of them.
    for src in ["(object) $a", "(real) $a", "(unset) $a", "(void) $a"] {
        assert_eq!(lowered(src), ArgValue::Other, "`{src}`");
    }
}

#[test]
fn the_node_is_total_over_its_operand() {
    // The whole design: an operand this vocabulary cannot spell arrives as
    // `Other` INSIDE the cast rather than widening the expression, because the
    // cast's answer is the operator's before it is the operand's.
    match lowered("(int) $a->b->c") {
        ArgValue::Cast { target, operand } => {
            assert_eq!(target, CastTarget::Int);
            assert_eq!(*operand, ArgValue::Other);
        }
        other => panic!("lowered to {other:?}"),
    }
}

#[test]
fn the_operand_keeps_its_own_lowering() {
    // Structural, not folded: a literal operand stays a literal here and the
    // fold happens at the walk's literal seam, which has the env.
    match lowered("(int) 5.25") {
        ArgValue::Cast { target, operand } => {
            assert_eq!(target, CastTarget::Int);
            assert_eq!(*operand, ArgValue::Float(5.25));
        }
        other => panic!("lowered to {other:?}"),
    }
    // Nested casts nest.
    match lowered("(string) (int) $a") {
        ArgValue::Cast { target: CastTarget::String, operand } => match *operand {
            ArgValue::Cast { target: CastTarget::Int, operand: inner } => {
                assert_eq!(*inner, ArgValue::Var("a".to_owned()));
            }
            other => panic!("inner lowered to {other:?}"),
        },
        other => panic!("lowered to {other:?}"),
    }
}

#[test]
fn a_cast_renders_as_the_canonical_spelling() {
    // One spelling per target in a diagnostic, whichever token was written.
    assert_eq!(lowered("(integer) $a").render(), "(int) $a");
    assert_eq!(lowered("(binary) $a").render(), "(string) $a");
    assert_eq!(lowered("(array) 1").render(), "(array) 1");
}
