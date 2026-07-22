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

// ---- ADR-0005/0006: `#[\Steins\Pure]` envelope attribute recognition -------

use steins_syntax::EffectOrigin;

/// The single function's `pure_envelope` presence.
fn is_pure(src: &str) -> bool {
    let tree = SourceTree::parse(src);
    tree.functions().iter().find(|f| f.name == "f").is_some_and(|f| f.pure_envelope.is_some())
}

#[test]
fn recognizes_fully_and_semi_qualified_pure() {
    assert!(is_pure("<?php #[\\Steins\\Pure] function f(): void {}"), "fully-qualified");
    assert!(is_pure("<?php #[Steins\\Pure] function f(): void {}"), "qualified");
    // Case-insensitive (PHP class names).
    assert!(is_pure("<?php #[\\steins\\pure] function f(): void {}"), "case-insensitive");
}

#[test]
fn bare_pure_recognized_only_with_use() {
    assert!(
        is_pure("<?php\nuse Steins\\Pure;\n#[Pure] function f(): void {}"),
        "bare #[Pure] with `use Steins\\Pure` binds"
    );
    assert!(
        is_pure("<?php\nuse Steins\\Pure as P;\n#[P] function f(): void {}"),
        "aliased #[P] with `use Steins\\Pure as P` binds"
    );
    // The JetBrains collision guard: bare #[Pure] WITHOUT the use does not match.
    assert!(!is_pure("<?php #[Pure] function f(): void {}"), "bare #[Pure] without use");
    // An alias binds only its own name, not the class's bare last segment.
    assert!(
        !is_pure("<?php\nuse Steins\\Pure as P;\n#[Pure] function f(): void {}"),
        "aliasing to P does not also bind Pure"
    );
}

#[test]
fn foreign_pure_attributes_do_not_match() {
    assert!(!is_pure("<?php #[JetBrains\\PhpStorm\\Pure] function f(): void {}"));
    assert!(!is_pure("<?php #[Some\\Other\\Pure] function f(): void {}"));
    assert!(!is_pure("<?php function f(): void {}"), "no attribute at all");
}

#[test]
fn scans_effect_origins_across_control_flow() {
    // echo nested in an if, a builtin call, and a same-file user call.
    let src = "<?php #[\\Steins\\Pure] function f(): void { if (true) { echo 1; } rand(); g(); }\nfunction g(): void {}";
    let tree = SourceTree::parse(src);
    let f = tree.functions().iter().find(|f| f.name == "f").unwrap();
    let mut echo = 0;
    let mut calls = Vec::new();
    for o in &f.effect_origins {
        match o {
            EffectOrigin::Output { keyword, .. } => {
                assert_eq!(*keyword, "echo");
                echo += 1;
            }
            EffectOrigin::Call { name, .. } => calls.push(name.clone()),
            EffectOrigin::Exit { .. } => panic!("no exit expected"),
        }
    }
    assert_eq!(echo, 1, "echo inside the if is found");
    assert!(calls.contains(&"rand".to_owned()));
    assert!(calls.contains(&"g".to_owned()));
}

#[test]
fn scans_exit_and_die() {
    let src = "<?php function f(): void { exit(); }\nfunction g(): void { die(1); }";
    let tree = SourceTree::parse(src);
    let f = tree.functions().iter().find(|x| x.name == "f").unwrap();
    assert!(matches!(f.effect_origins.first(), Some(EffectOrigin::Exit { keyword: "exit", .. })));
    let g = tree.functions().iter().find(|x| x.name == "g").unwrap();
    assert!(matches!(g.effect_origins.first(), Some(EffectOrigin::Exit { keyword: "die", .. })));
}

#[test]
fn nested_closure_bodies_are_not_scanned() {
    // The echo is inside a closure — a separate scope — so it is NOT an origin
    // of the outer function (closures deferred this slice).
    let src = "<?php function f(): void { $g = function () { echo 1; }; }";
    let tree = SourceTree::parse(src);
    let f = &tree.functions()[0];
    assert!(
        !f.effect_origins.iter().any(|o| matches!(o, EffectOrigin::Output { .. })),
        "closure-nested echo is not the outer function's effect"
    );
}
