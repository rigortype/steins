use steins_syntax::{ArgValue, ParamType, ScalarType, SourceTree};

#[test]
fn lowers_functions_calls_and_strict() {
    let src = "<?php\ndeclare(strict_types=1);\nfunction width(int $w): int { return $w; }\nwidth(\"abc\");\nwidth(5);\n";
    let tree = SourceTree::parse(src);
    assert!(tree.has_strict_types());
    assert_eq!(tree.functions().len(), 1);
    let f = &tree.functions()[0];
    assert_eq!(f.name, "width");
    assert_eq!(f.params.len(), 1);
    assert_eq!(f.params[0].name, "w");
    assert_eq!(f.params[0].ty, Some(ParamType { scalar: ScalarType::Int, nullable: false }));
    assert_eq!(tree.calls().len(), 2);
    assert_eq!(tree.calls()[0].callee.as_deref(), Some("width"));
    assert_eq!(tree.calls()[0].args[0].value, ArgValue::Str("abc".into()));
    assert_eq!(tree.calls()[1].args[0].value, ArgValue::Int(5));
    let p = tree.position(tree.calls()[0].args[0].span.start);
    assert_eq!(p.line, 4);
}

#[test]
fn parse_error_no_panic() {
    let tree = SourceTree::parse("<?php function broken( { echo 1;");
    let _ = tree.parse_errors();
    let _ = tree.functions();
}

#[test]
fn lowers_scopes_trace_and_poison() {
    use steins_syntax::StmtKind;

    // Top-level scope + one function scope.
    let src = "<?php\nfunction price(): string { return \"abc\"; }\n$w = \"abc\";\nwidth($w);\n";
    let tree = SourceTree::parse(src);
    assert_eq!(tree.scopes().len(), 2, "top-level + price()");

    let top = tree.scopes().iter().find(|s| s.function_name.is_none()).unwrap();
    assert!(!top.poisoned);
    // The function *declaration* is a Barrier at top level; then the assign and call.
    let kinds: Vec<&StmtKind> = top.stmts.iter().map(|s| &s.kind).collect();
    assert!(matches!(kinds[0], StmtKind::Barrier), "nested fn decl → Barrier");
    assert!(matches!(kinds[1], StmtKind::Assign { var, .. } if var == "w"));
    assert!(matches!(kinds[2], StmtKind::Call(_)));
    // `width($w)` hands `$w` to a call → invalidated after the statement.
    assert_eq!(top.stmts[2].invalidated, vec!["w".to_owned()]);

    // price() is a constant function: body is exactly `[Return(literal)]`.
    let price = tree.scopes().iter().find(|s| s.function_name.as_deref() == Some("price")).unwrap();
    assert!(!price.poisoned);
    assert_eq!(price.stmts.len(), 1);
    assert!(matches!(&price.stmts[0].kind, StmtKind::Return { value, .. } if value.is_literal()));
}

#[test]
fn poison_markers_are_detected() {
    for (src, why) in [
        ("<?php $r = &$w; width($w);", "reference assignment"),
        ("<?php extract($d); width($w);", "extract"),
        ("<?php compact('w'); width($w);", "compact"),
        ("<?php global $w; width($w);", "global"),
        ("<?php static $w = 1; width($w);", "static var"),
        ("<?php $$w = 1; width($w);", "variable-variable"),
        ("<?php $f = function () use (&$w) {}; width($w);", "by-ref closure capture"),
    ] {
        let tree = SourceTree::parse(src);
        let top = tree.scopes().iter().find(|s| s.function_name.is_none()).unwrap();
        assert!(top.poisoned, "{why} should poison the top-level scope");
    }
}
