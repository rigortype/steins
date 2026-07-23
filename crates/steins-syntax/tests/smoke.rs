use steins_syntax::{ArgValue, CommentKind, NativeType, ScalarType, SourceTree, TypeMember};

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
    assert_eq!(
        f.params[0].ty,
        Some(NativeType { members: vec![TypeMember::Scalar(ScalarType::Int)], nullable: false })
    );
    // The native scalar return type `: int` is lowered too.
    assert_eq!(
        f.ret,
        Some(NativeType { members: vec![TypeMember::Scalar(ScalarType::Int)], nullable: false })
    );
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
            EffectOrigin::Call { name, .. } => calls.push(name.simple().to_owned()),
            EffectOrigin::Exit { .. } => panic!("no exit expected"),
            EffectOrigin::MethodCall { .. } => panic!("no method call expected"),
            EffectOrigin::Opaque { .. } => panic!("no opaque call expected"),
            EffectOrigin::HigherOrder { .. } => panic!("no higher-order call expected"),
            EffectOrigin::Callback { .. } => panic!("no callback call expected"),
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
    assert_eq!(foo.parent.as_ref().map(|r| r.raw.as_str()), Some("Bar"));
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
fn interfaces_and_enums_lowered_traits_not() {
    // Interfaces are lowered (ADR-0033 Liskov), marked is_interface; enums are
    // lowered too (ADR-0043 object/method world), marked is_enum; traits stay
    // unlowered.
    let src = "<?php\ninterface I {}\ntrait T {}\nenum E { case A; }\nclass C {}\n";
    let tree = SourceTree::parse(src);
    assert_eq!(tree.classes().len(), 3, "the class, interface, and enum are lowered");
    let c = tree.classes().iter().find(|d| d.name == "C").unwrap();
    assert!(!c.is_interface && !c.is_enum);
    let i = tree.classes().iter().find(|d| d.name == "I").unwrap();
    assert!(i.is_interface, "interface I is marked is_interface");
    let e = tree.classes().iter().find(|d| d.name == "E").unwrap();
    assert!(e.is_enum, "enum E is marked is_enum");
    assert!(e.is_final, "an enum is implicitly final");
    // The unit case is recorded; no trait is lowered.
    assert_eq!(e.enum_cases.len(), 1);
    assert_eq!(e.enum_cases[0].name, "A");
    assert!(tree.classes().iter().all(|d| d.name != "T"), "the trait stays unlowered");
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
    assert!(matches!(value, ArgValue::New(c, _) if c.raw == "Foo"), "value is New(Foo)");
    // The RHS also carries a constructor CallExpr for arg-checking.
    let call = call.as_ref().expect("ctor call carried");
    assert!(matches!(&call.receiver, Callee::Construct { class } if class.raw == "Foo"));
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
    assert!(receivers.iter().any(|r| matches!(r, Callee::Method { receiver: Receiver::This, method, .. } if method == "m")));
    assert!(receivers.iter().any(|r| matches!(r, Callee::Static { class: StaticClass::SelfKw, method } if method == "s")));
    assert!(receivers.iter().any(|r| matches!(r, Callee::Static { class: StaticClass::Parent, method } if method == "p")));
    assert!(receivers.iter().any(|r| matches!(r, Callee::Static { class: StaticClass::Static, method } if method == "x")));
    assert!(receivers.iter().any(|r| matches!(r, Callee::Static { class: StaticClass::Named(c), method } if c.raw == "Bar" && method == "b")));
    assert!(receivers.iter().any(|r| matches!(r, Callee::Method { receiver: Receiver::Var(v), method, .. } if v == "v" && method == "d")));
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

#[test]
fn comments_are_exposed_with_kind_span_and_text() {
    let src = "<?php\n// line one\n# hashed\n/* block */\nfunction f(): void {}\n";
    let tree = SourceTree::parse(src);
    let comments = tree.comments();
    assert_eq!(comments.len(), 3, "three comment trivia, got: {comments:?}");
    assert_eq!(comments[0].kind, CommentKind::Line);
    assert!(comments[0].text.contains("line one"));
    assert_eq!(comments[1].kind, CommentKind::Hash);
    assert_eq!(comments[2].kind, CommentKind::Block);
    // The span resolves to the right line.
    assert_eq!(tree.position(comments[0].span.start).line, 2);
}

#[test]
fn is_line_leading_distinguishes_trailing_from_own_line() {
    // A comment alone on its line leads; one trailing code does not.
    let src = "<?php\n// leading\n$x = 1; // trailing\n";
    let tree = SourceTree::parse(src);
    let leading = &tree.comments()[0];
    let trailing = &tree.comments()[1];
    assert!(tree.is_line_leading(leading.span.start), "own-line comment leads");
    assert!(!tree.is_line_leading(trailing.span.start), "trailing comment does not lead");
}

// --- ADR-0043 stage 1: object types, enums, class-const/enum-case values ------

#[test]
fn object_param_lowers_to_instance_member() {
    // A class type hint lowers to a namespace-resolved `Instance` member
    // (ADR-0043) — lowercase `fqn` for matching, source-cased `display` for
    // diagnostics — no longer collapsing the whole type to `None`.
    let src = "<?php\nnamespace App;\nuse Other\\Bar;\nfunction f(Foo $a, Bar $b, \\Ns\\Baz $c): void {}\n";
    let tree = SourceTree::parse(src);
    let f = &tree.functions()[0];
    let member = |p: usize| match &tree.functions()[0].params[p].ty {
        Some(NativeType { members, nullable: false }) if members.len() == 1 => match &members[0] {
            TypeMember::Instance { fqn, display } => (fqn.clone(), display.clone()),
            other => panic!("expected Instance, got {other:?}"),
        },
        other => panic!("expected single Instance member, got {other:?}"),
    };
    // Unqualified `Foo` resolves against the current namespace `App`.
    assert_eq!(member(0), ("app\\foo".into(), "App\\Foo".into()));
    // `Bar` resolves through the `use Other\Bar` import.
    assert_eq!(member(1), ("other\\bar".into(), "Other\\Bar".into()));
    // A fully-qualified `\Ns\Baz` passes through (leading `\` trimmed; `fqn`
    // lowercased, `display` source-cased).
    assert_eq!(member(2), ("ns\\baz".into(), "Ns\\Baz".into()));
    assert!(f.ret.is_none(), "a `void` return stays unlowered");
}

#[test]
fn object_scalar_union_is_one_shape() {
    // `Foo|null` and `A|B` are now a single union shape mixing objects and scalars.
    let src = "<?php\nfunction f(?Foo $a, int|Bar $b): void {}\n";
    let tree = SourceTree::parse(src);
    let a = tree.functions()[0].params[0].ty.as_ref().unwrap();
    assert!(a.nullable, "`?Foo` is nullable");
    assert_eq!(
        a.members,
        vec![TypeMember::Instance { fqn: "foo".into(), display: "Foo".into() }]
    );
    assert!(a.has_instance());
    let b = tree.functions()[0].params[1].ty.as_ref().unwrap();
    assert_eq!(
        b.members,
        vec![
            TypeMember::Scalar(ScalarType::Int),
            TypeMember::Instance { fqn: "bar".into(), display: "Bar".into() }
        ]
    );
    assert!(b.has_instance(), "a union mixing a scalar and an object is object-bearing");
    assert!(!b.nullable);
}

#[test]
fn self_static_parent_and_intersection_stay_unlowered() {
    // `self`/`static`/`parent` remain silent (None); an intersection stays None too.
    let src = "<?php\nclass C {\n  function a(): self { return $this; }\n  function b(): static { return $this; }\n  function c(parent $p): void {}\n  function d(A&B $x): void {}\n}\n";
    let tree = SourceTree::parse(src);
    let c = tree.classes().iter().find(|d| d.name == "C").unwrap();
    let m = |name: &str| c.methods.iter().find(|m| m.name == name).unwrap();
    assert!(m("a").ret.is_none(), "self return stays None");
    assert!(m("b").ret.is_none(), "static return stays None");
    assert!(m("c").params[0].ty.is_none(), "parent param stays None");
    assert!(m("d").params[0].ty.is_none(), "an intersection stays None (v1 deferral)");
}

#[test]
fn enum_lowered_with_backing_and_cases() {
    // A backed enum records its backing scalar, cases (with literal backed values),
    // and implemented interfaces; it is final and marked is_enum.
    let src = "<?php\nnamespace App;\nenum Suit: string implements HasLabel {\n  case Hearts = 'H';\n  case Spades = 'S';\n}\n";
    let tree = SourceTree::parse(src);
    let e = tree.classes().iter().find(|d| d.name == "Suit").unwrap();
    assert!(e.is_enum && e.is_final && !e.is_interface);
    assert_eq!(e.fqn, "app\\suit");
    assert_eq!(e.enum_backing, Some(ScalarType::String));
    assert_eq!(e.enum_cases.len(), 2);
    assert_eq!(e.enum_cases[0].name, "Hearts");
    assert_eq!(e.enum_cases[0].value, Some(ArgValue::Str("H".into())));
    assert_eq!(e.implements.len(), 1, "the implemented interface is recorded");
    assert!(e.methods.is_empty(), "enum method bodies are not lowered in v1");
    // A pure (unit) enum records no backing.
    let src2 = "<?php\nenum Dir { case Up; case Down; }\n";
    let tree2 = SourceTree::parse(src2);
    let d = tree2.classes().iter().find(|d| d.name == "Dir").unwrap();
    assert!(d.is_enum && d.enum_backing.is_none());
    assert_eq!(d.enum_cases.len(), 2);
    assert!(d.enum_cases[0].value.is_none(), "a unit case has no backed value");
}

#[test]
fn class_const_access_lowers_to_class_const_value() {
    // `Class::CONST` / `Enum::Case` lower to the uniform ClassConst value (an
    // unproven object-world value), no longer erased to Other.
    let src = "<?php\nf(Foo::BAR, self::BAZ, $x::DYN, Suit::Hearts);\n";
    let tree = SourceTree::parse(src);
    let args = &tree.calls()[0].args;
    match &args[0].value {
        ArgValue::ClassConst(_, name) => assert_eq!(name, "BAR"),
        other => panic!("expected ClassConst, got {other:?}"),
    }
    match &args[1].value {
        ArgValue::ClassConst(_, name) => assert_eq!(name, "BAZ"),
        other => panic!("expected ClassConst for self::BAZ, got {other:?}"),
    }
    // A dynamic class expression `$x::DYN` is not statically named → Other.
    assert_eq!(args[2].value, ArgValue::Other);
    match &args[3].value {
        ArgValue::ClassConst(_, name) => assert_eq!(name, "Hearts"),
        other => panic!("expected ClassConst, got {other:?}"),
    }
}

// ---- ADR-0046 §2: dynamism sites (eval / include / require) ----------------

use steins_syntax::{DynamismKind, IncludePath};

#[test]
fn eval_is_collected_as_a_dynamism_site() {
    let tree = SourceTree::parse("<?php\neval('foo(42)');\n");
    assert!(tree.contains_eval());
    let sites = tree.dynamism_sites();
    assert_eq!(sites.len(), 1);
    assert!(matches!(sites[0].kind, DynamismKind::Eval));
    // The site's starting line is the vouching key.
    assert_eq!(tree.position(sites[0].span.start).line, 2);
}

#[test]
fn eval_inside_a_function_body_is_collected_file_wide() {
    // Unlike the per-scope poison flag, dynamism collection descends into bodies.
    let tree = SourceTree::parse("<?php\nfunction f() { eval('x();'); }\n");
    assert!(tree.contains_eval());
    assert_eq!(tree.dynamism_sites().len(), 1);
}

#[test]
fn include_path_shapes_lower_as_expected() {
    let cases: &[(&str, IncludePath)] = &[
        ("<?php require 'inc/util.php';", IncludePath::Literal("inc/util.php".to_owned())),
        ("<?php include_once __DIR__ . '/a.php';", IncludePath::DirRelative("/a.php".to_owned())),
        ("<?php require __DIR__ . '/a' . '/b.php';", IncludePath::DirRelative("/a/b.php".to_owned())),
        ("<?php require 'a' . 'b.php';", IncludePath::Literal("ab.php".to_owned())),
        ("<?php require $page;", IncludePath::Unproven),
        ("<?php require dirname(__FILE__) . '/x.php';", IncludePath::Unproven),
    ];
    for (src, want) in cases {
        let tree = SourceTree::parse(src);
        let sites = tree.dynamism_sites();
        assert_eq!(sites.len(), 1, "`{src}`");
        match &sites[0].kind {
            DynamismKind::Include(ip) => assert_eq!(ip, want, "`{src}`"),
            other => panic!("`{src}`: expected include, got {other:?}"),
        }
    }
}

#[test]
fn all_four_import_keywords_are_collected() {
    let tree = SourceTree::parse(
        "<?php\ninclude 'a.php';\ninclude_once 'b.php';\nrequire 'c.php';\nrequire_once 'd.php';\n",
    );
    assert_eq!(tree.dynamism_sites().len(), 4);
    assert!(!tree.contains_eval());
}

#[test]
fn a_clean_file_has_no_dynamism_sites() {
    let tree = SourceTree::parse("<?php\nfunction f(int $x): int { return $x; }\nf(1);\n");
    assert!(tree.dynamism_sites().is_empty());
    assert!(!tree.contains_eval());
}
