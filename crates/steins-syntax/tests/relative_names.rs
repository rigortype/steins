//! PHP `namespace\X` references lower to [`RefKind::Relative`] with the prefix
//! stripped, then resolve against the enclosing namespace (ADR-0049 A8). They
//! never produce a doubled `namespace` segment.

use steins_syntax::{Callee, Receiver, RefKind, SourceTree, StaticClass};

fn method_calls(tree: &SourceTree) -> impl Iterator<Item = &steins_syntax::CallExpr> {
    tree.scopes().iter().flat_map(|s| s.method_calls.iter())
}

/// Extract the `new` class reference from a method receiver.
fn new_class_ref(tree: &SourceTree) -> steins_syntax::NameRef {
    method_calls(tree)
        .find_map(|c| match &c.receiver {
            Callee::Method { receiver: Receiver::New { class, .. }, .. } => Some(class.clone()),
            _ => None,
        })
        .expect("a new-receiver method call")
}

fn static_class_ref(tree: &SourceTree) -> steins_syntax::NameRef {
    method_calls(tree)
        .find_map(|c| match &c.receiver {
            Callee::Static { class: StaticClass::Named(r), .. } => Some(r.clone()),
            _ => None,
        })
        .expect("a static call")
}

fn fn_ref(tree: &SourceTree) -> steins_syntax::NameRef {
    tree.calls()
        .iter()
        .find_map(|c| match &c.receiver {
            Callee::Function(_) => c.callee_ref.clone(),
            _ => None,
        })
        .expect("a function call with a callee ref")
}

#[test]
fn relative_function_call_lowers_to_relative_kind() {
    let tree = SourceTree::parse("<?php\nnamespace App;\nnamespace\\foo();\n");
    let r = fn_ref(&tree);
    assert_eq!(r.kind, RefKind::Relative);
    assert_eq!(r.raw, "foo", "the `namespace\\` prefix must be stripped");
}

#[test]
fn relative_new_lowers_to_relative_kind() {
    let tree = SourceTree::parse("<?php\nnamespace App;\n(new namespace\\Bar())->m();\n");
    let r = new_class_ref(&tree);
    assert_eq!(r.kind, RefKind::Relative);
    assert_eq!(r.raw, "Bar");
}

#[test]
fn relative_class_resolves_to_current_namespace() {
    let tree = SourceTree::parse("<?php\nnamespace App;\n(new namespace\\Bar())->m();\n");
    let fqn = tree.resolve_class_fqn(&new_class_ref(&tree));
    assert_eq!(fqn, "App\\Bar");
    assert_ne!(fqn, "App\\namespace\\Bar", "the doubled prefix must be gone");
}

#[test]
fn relative_class_in_nested_namespace_segment_resolves() {
    let tree = SourceTree::parse("<?php\nnamespace App;\n(new namespace\\Sub\\Bar())->m();\n");
    let r = new_class_ref(&tree);
    assert_eq!(r.kind, RefKind::Relative);
    assert_eq!(r.raw, "Sub\\Bar");
    assert_eq!(tree.resolve_class_fqn(&r), "App\\Sub\\Bar");
}

#[test]
fn relative_static_call_class_resolves_to_current_namespace() {
    let tree = SourceTree::parse("<?php\nnamespace App;\nnamespace\\Registry::make();\n");
    let fqn = tree.resolve_class_fqn(&static_class_ref(&tree));
    assert_eq!(fqn, "App\\Registry");
}

#[test]
fn relative_in_global_namespace_is_the_bare_remainder() {
    let tree = SourceTree::parse("<?php\n(new namespace\\Bar())->m();\n");
    let r = new_class_ref(&tree);
    assert_eq!(r.kind, RefKind::Relative);
    assert_eq!(tree.resolve_class_fqn(&r), "Bar");
}

#[test]
fn relative_keyword_prefix_is_case_insensitive() {
    let tree = SourceTree::parse("<?php\nnamespace App;\n(new NAMESPACE\\Bar())->m();\n");
    let r = new_class_ref(&tree);
    assert_eq!(r.kind, RefKind::Relative);
    assert_eq!(tree.resolve_class_fqn(&r), "App\\Bar");
}

#[test]
fn ordinary_qualified_name_is_unaffected() {
    let tree = SourceTree::parse("<?php\nnamespace App;\n(new Sub\\Bar())->m();\n");
    let r = new_class_ref(&tree);
    assert_eq!(r.kind, RefKind::Qualified);
    assert_eq!(tree.resolve_class_fqn(&r), "App\\Sub\\Bar");
}

