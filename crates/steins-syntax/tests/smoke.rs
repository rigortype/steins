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

/// Whether the single function `f` carries a recognized `Pure` envelope (an
/// effect envelope with the empty label set).
fn is_pure(src: &str) -> bool {
    let tree = SourceTree::parse(src);
    tree.functions()
        .iter()
        .find(|f| f.name == "f")
        .and_then(|f| f.effect_envelope.as_ref())
        .is_some_and(|e| e.labels.is_empty())
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

// ---- ADR-0018: `#[\Steins\Effect(...)]` recognition + lowering ------------

use steins_syntax::EffectEnvelope;

/// The recognized envelope on function `f`, if any.
fn envelope(src: &str) -> Option<EffectEnvelope> {
    SourceTree::parse(src)
        .functions()
        .iter()
        .find(|f| f.name == "f")
        .and_then(|f| f.effect_envelope.clone())
}

#[test]
fn recognizes_effect_with_string_literal_labels() {
    let e = envelope("<?php #[\\Steins\\Effect('io', 'nondet.time')] function f(): void {}")
        .expect("recognized");
    assert_eq!(e.labels, vec!["io".to_owned(), "nondet.time".to_owned()]);

    // Qualified spelling and case-insensitivity.
    let e = envelope("<?php #[Steins\\Effect('io.fs.read')] function f(): void {}").expect("qualified");
    assert_eq!(e.labels, vec!["io.fs.read".to_owned()]);
}

#[test]
fn recognizes_effect_via_use_alias() {
    let e = envelope("<?php\nuse Steins\\Effect;\n#[Effect('io')] function f(): void {}")
        .expect("bare with use");
    assert_eq!(e.labels, vec!["io".to_owned()]);

    let e = envelope("<?php\nuse Steins\\Effect as Fx;\n#[Fx('nondet')] function f(): void {}")
        .expect("aliased");
    assert_eq!(e.labels, vec!["nondet".to_owned()]);

    // Bare #[Effect(...)] without the use is not the Steins envelope.
    assert!(envelope("<?php #[Effect('io')] function f(): void {}").is_none());
}

#[test]
fn effect_with_non_literal_args_is_unrecognized() {
    // Class-constant argument — not resolvable without constant resolution.
    assert!(
        envelope("<?php #[\\Steins\\Effect(Effects::IO)] function f(): void {}").is_none(),
        "class-constant arg → whole attribute ignored"
    );
    // Concatenation and named args likewise.
    assert!(envelope("<?php #[\\Steins\\Effect('io' . '.fs')] function f(): void {}").is_none());
    assert!(envelope("<?php #[\\Steins\\Effect(label: 'io')] function f(): void {}").is_none());
    // A non-string literal (int) is also not a label.
    assert!(envelope("<?php #[\\Steins\\Effect(42)] function f(): void {}").is_none());
}

#[test]
fn pure_wins_over_effect_when_both_present() {
    let e = envelope("<?php #[\\Steins\\Pure]\n#[\\Steins\\Effect('io')] function f(): void {}")
        .expect("recognized");
    assert!(e.labels.is_empty(), "Pure (empty upper bound) wins the contradiction");
    // Order-independent: Effect first, Pure second.
    let e = envelope("<?php #[\\Steins\\Effect('io')]\n#[\\Steins\\Pure] function f(): void {}")
        .expect("recognized");
    assert!(e.labels.is_empty());
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
            EffectOrigin::MethodCall { .. } => panic!("no method call expected"),
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

// ---- Class / method lowering (class-world extension) ----------------------

use steins_syntax::{Callee, ClassDecl, Receiver, ScopeOwner, StaticClass, StmtKind, Visibility};

fn class<'a>(tree: &'a SourceTree, name: &str) -> &'a ClassDecl {
    tree.classes().iter().find(|c| c.name == name).expect("class present")
}

#[test]
fn lowers_class_and_method_shape() {
    let src = "<?php\nfinal class Foo extends Bar {\n  use SomeTrait;\n  public function __construct(int $w) {}\n  protected static final function s(string $x): void {}\n  private function p(): void {}\n  abstract public function a(): void;\n}\n";
    let tree = SourceTree::parse(src);
    let foo = class(&tree, "Foo");
    assert!(foo.is_final);
    assert_eq!(foo.parent.as_deref(), Some("Bar"));
    assert!(foo.uses_traits, "`use SomeTrait;` sets uses_traits");
    assert_eq!(foo.methods.len(), 4);

    let ctor = foo.methods.iter().find(|m| m.is_constructor).unwrap();
    assert_eq!(ctor.name, "__construct");
    assert_eq!(ctor.visibility, Visibility::Public);
    assert_eq!(ctor.params.len(), 1);

    let s = foo.methods.iter().find(|m| m.name == "s").unwrap();
    assert_eq!(s.visibility, Visibility::Protected);
    assert!(s.is_static && s.is_final);

    let p = foo.methods.iter().find(|m| m.name == "p").unwrap();
    assert_eq!(p.visibility, Visibility::Private);

    let a = foo.methods.iter().find(|m| m.name == "a").unwrap();
    assert!(a.is_abstract);
}

#[test]
fn interfaces_traits_enums_are_not_lowered_as_classes() {
    let src = "<?php\ninterface I {}\ntrait T {}\nenum E { case A; }\nclass C {}\n";
    let tree = SourceTree::parse(src);
    assert_eq!(tree.classes().len(), 1, "only the class is lowered");
    assert_eq!(tree.classes()[0].name, "C");
}

#[test]
fn method_bodies_become_method_scopes() {
    let src = "<?php\nclass Foo {\n  public function go(): void { $x = 1; }\n  abstract public function skip(): void;\n}\n";
    let tree = SourceTree::parse(src);
    let method_scopes: Vec<_> = tree
        .scopes()
        .iter()
        .filter(|s| matches!(&s.owner, ScopeOwner::Method { .. }))
        .collect();
    // Only the concrete method gets a scope; the abstract one has no body.
    assert_eq!(method_scopes.len(), 1);
    assert!(matches!(
        &method_scopes[0].owner,
        ScopeOwner::Method { class, method } if class == "Foo" && method == "go"
    ));
    // A method scope is not a free-function scope.
    assert!(method_scopes[0].function_name.is_none());
}

#[test]
fn lowers_new_expression_as_class_fact_rvalue() {
    let src = "<?php $x = new Foo(\"abc\");";
    let tree = SourceTree::parse(src);
    let top = tree.scopes().iter().find(|s| s.function_name.is_none()).unwrap();
    let StmtKind::Assign { value, call, .. } = &top.stmts[0].kind else { panic!("assign") };
    assert!(matches!(value, ArgValue::New(c, _) if c == "Foo"), "value is New(Foo)");
    // The RHS also carries a constructor CallExpr for arg-checking.
    let call = call.as_ref().expect("ctor call carried");
    assert!(matches!(&call.receiver, Callee::Construct { class } if class == "Foo"));
    assert_eq!(call.args[0].value, ArgValue::Str("abc".into()));
}

#[test]
fn lowers_method_and_static_call_receivers() {
    let src = "<?php\nclass Foo {\n  public function go(): void { $this->m(); self::s(); parent::p(); static::x(); Bar::b(); $v->d(); }\n}\n";
    let tree = SourceTree::parse(src);
    let go = tree
        .scopes()
        .iter()
        .find(|s| matches!(&s.owner, ScopeOwner::Method { method, .. } if method == "go"))
        .unwrap();
    let receivers: Vec<&Callee> = go
        .stmts
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::Call(c) => Some(&c.receiver),
            _ => None,
        })
        .collect();
    assert!(receivers.iter().any(|r| matches!(r, Callee::Method { receiver: Receiver::This, method } if method == "m")));
    assert!(receivers.iter().any(|r| matches!(r, Callee::Static { class: StaticClass::SelfKw, method } if method == "s")));
    assert!(receivers.iter().any(|r| matches!(r, Callee::Static { class: StaticClass::Parent, method } if method == "p")));
    assert!(receivers.iter().any(|r| matches!(r, Callee::Static { class: StaticClass::Static, method } if method == "x")));
    assert!(receivers.iter().any(|r| matches!(r, Callee::Static { class: StaticClass::Named(c), method } if c == "Bar" && method == "b")));
    assert!(receivers.iter().any(|r| matches!(r, Callee::Method { receiver: Receiver::Var(v), method } if v == "v" && method == "d")));
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
