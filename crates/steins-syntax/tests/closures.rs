//! Syntax-layer acceptance tests for the closure wave (ADR-0033): closures,
//! arrow functions, and first-class callables lower to `ArgValue::Closure` and
//! get their own `ScopeOwner::Closure` scope carrying params/effects/throws, with
//! by-value captures recorded and by-ref `use (&$x)` preserving poison.

use steins_syntax::{ArgValue, ClosureRef, Scope, ScopeOwner, SourceTree, StmtKind};

/// The closure scopes in a parsed file (ADR-0033 own-scope discipline).
fn closure_scopes(tree: &SourceTree) -> Vec<&Scope> {
    tree.scopes()
        .iter()
        .filter(|s| matches!(s.owner, ScopeOwner::Closure { .. }))
        .collect()
}

/// The `ArgValue` assigned to `$var` in the top-level scope's trace.
fn assigned_value<'a>(tree: &'a SourceTree, var: &str) -> Option<&'a ArgValue> {
    tree.scopes().iter().find_map(|s| {
        s.stmts.iter().find_map(|st| match &st.kind {
            StmtKind::Assign { var: v, value, .. } if v == var => Some(value),
            _ => None,
        })
    })
}

#[test]
fn closure_expression_lowers_to_closure_value_with_own_scope() {
    let src = "<?php\n$f = function () { return 1; };\n";
    let tree = SourceTree::parse(src);
    // The rvalue is a Closure value naming an anonymous scope.
    match assigned_value(&tree, "f") {
        Some(ArgValue::Closure(ClosureRef::Anonymous { captures, .. })) => {
            assert!(captures.is_empty(), "no use() → no captures");
        }
        other => panic!("expected Closure value, got {other:?}"),
    }
    // A closure scope was built for the body.
    assert_eq!(closure_scopes(&tree).len(), 1, "one closure scope");
}

#[test]
fn arrow_function_lowers_and_auto_captures_free_vars() {
    // fn () => $x auto-captures $x by value (arrow semantics).
    let src = "<?php\n$x = 1;\n$f = fn () => $x;\n";
    let tree = SourceTree::parse(src);
    match assigned_value(&tree, "f") {
        Some(ArgValue::Closure(ClosureRef::Anonymous { captures, .. })) => {
            assert_eq!(captures, &vec!["x".to_owned()], "arrow captures free var $x");
        }
        other => panic!("expected Closure value, got {other:?}"),
    }
    assert_eq!(closure_scopes(&tree).len(), 1);
}

#[test]
fn arrow_params_are_not_captures() {
    // fn ($w) => width($w): $w is a PARAM, not a capture.
    let src = "<?php\n$f = fn (int $w) => width($w);\n";
    let tree = SourceTree::parse(src);
    match assigned_value(&tree, "f") {
        Some(ArgValue::Closure(ClosureRef::Anonymous { captures, .. })) => {
            assert!(captures.is_empty(), "$w is a param, not a capture: {captures:?}");
        }
        other => panic!("expected Closure value, got {other:?}"),
    }
    // The closure scope carries the typed param $w.
    let cs = closure_scopes(&tree);
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].params.len(), 1);
    assert_eq!(cs[0].params[0].name, "w");
    assert!(cs[0].params[0].ty.is_some(), "int $w param has a native type");
}

#[test]
fn closure_use_by_value_records_captures() {
    let src = "<?php\n$a = 1; $b = 2;\n$f = function () use ($a, $b) { return $a; };\n";
    let tree = SourceTree::parse(src);
    match assigned_value(&tree, "f") {
        Some(ArgValue::Closure(ClosureRef::Anonymous { captures, .. })) => {
            assert_eq!(captures, &vec!["a".to_owned(), "b".to_owned()]);
        }
        other => panic!("expected Closure value, got {other:?}"),
    }
}

#[test]
fn by_ref_use_poisons_enclosing_and_closure_scope() {
    // use (&$x) poisons BOTH the enclosing scope and the closure's own scope.
    let src = "<?php\n$x = 1;\n$f = function () use (&$x) { return $x; };\n";
    let tree = SourceTree::parse(src);
    let top = tree
        .scopes()
        .iter()
        .find(|s| matches!(s.owner, ScopeOwner::TopLevel))
        .unwrap();
    assert!(top.poisoned, "by-ref use poisons the enclosing scope");
    let cs = closure_scopes(&tree);
    assert_eq!(cs.len(), 1);
    assert!(cs[0].poisoned, "by-ref use poisons the closure's own scope");
    // A by-ref capture is NOT recorded as a by-value capture name.
    match assigned_value(&tree, "f") {
        Some(ArgValue::Closure(ClosureRef::Anonymous { captures, .. })) => {
            assert!(captures.is_empty(), "by-ref use is not a by-value capture");
        }
        other => panic!("expected Closure value, got {other:?}"),
    }
}

#[test]
fn first_class_callable_lowers_to_function_name() {
    let src = "<?php\n$f = strtolower(...);\n";
    let tree = SourceTree::parse(src);
    match assigned_value(&tree, "f") {
        Some(ArgValue::Closure(ClosureRef::FunctionName(n))) => {
            assert_eq!(n.simple(), "strtolower");
        }
        other => panic!("expected first-class callable, got {other:?}"),
    }
    // A first-class callable of a NAMED function does not create a closure scope.
    assert_eq!(closure_scopes(&tree).len(), 0);
}

#[test]
fn method_first_class_callable_is_deferred_to_other() {
    // $obj->m(...) and Foo::m(...) are deferred this slice → Other, no scope.
    let src = "<?php\n$f = $obj->m(...);\n$g = Foo::m(...);\n";
    let tree = SourceTree::parse(src);
    assert!(matches!(assigned_value(&tree, "f"), Some(ArgValue::Other)));
    assert!(matches!(assigned_value(&tree, "g"), Some(ArgValue::Other)));
}

#[test]
fn closure_body_effect_and_throw_origins_are_captured() {
    // The closure scope records its own effect (echo → output) and throw origins.
    let src = "<?php\n$f = function () { echo 1; throw new \\RuntimeException(); };\n";
    let tree = SourceTree::parse(src);
    let cs = closure_scopes(&tree);
    assert_eq!(cs.len(), 1);
    assert!(!cs[0].effect_origins.is_empty(), "echo is an effect origin");
    assert!(!cs[0].throw_origins.is_empty(), "throw is a throw origin");
}

#[test]
fn nested_closures_each_get_a_scope() {
    let src = "<?php\n$f = function () { $g = fn () => 1; return $g; };\n";
    let tree = SourceTree::parse(src);
    assert_eq!(closure_scopes(&tree).len(), 2, "outer closure + inner arrow");
}

#[test]
fn closure_inside_function_body_gets_its_own_scope() {
    let src = "<?php\nfunction outer() { $f = fn () => 1; return $f; }\n";
    let tree = SourceTree::parse(src);
    // Function scope + closure scope both present.
    assert!(tree.scopes().iter().any(|s| matches!(&s.owner, ScopeOwner::Function(n) if n == "outer")));
    assert_eq!(closure_scopes(&tree).len(), 1);
}
