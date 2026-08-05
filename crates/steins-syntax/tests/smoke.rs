use steins_syntax::{ArgValue, ClassRef, CommentKind, NativeType, ScalarType, SourceTree, TypeMember};

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
fn whole_line_span_widens_only_lone_statements() {
    use steins_syntax::Span;

    // Line 2 ("  foo();") holds nothing but the statement: the span widens to
    // the line start and swallows the trailing newline. Line 3 shares its line
    // with a trailing comment: unchanged. Line 4 ends the file without a
    // newline: widened to the file end.
    let src = "<?php\n  foo();\nbar(); // t\n  baz();";
    let tree = SourceTree::parse(src);
    assert_eq!(tree.whole_line_span(Span { start: 8, end: 14 }), Span { start: 6, end: 15 });
    assert_eq!(tree.whole_line_span(Span { start: 15, end: 21 }), Span { start: 15, end: 21 });
    assert_eq!(tree.whole_line_span(Span { start: 29, end: 35 }), Span { start: 27, end: 35 });

    // CRLF line endings: the CR is part of the line break and is swallowed
    // with the LF.
    let crlf = SourceTree::parse("<?php\r\nfoo();\r\n");
    assert_eq!(crlf.whole_line_span(Span { start: 7, end: 13 }), Span { start: 7, end: 15 });

    // Code BEFORE the statement on its line: unchanged (deleting the line
    // would delete the sibling statement too).
    let two = SourceTree::parse("<?php\nfoo(); bar();\n");
    assert_eq!(two.whole_line_span(Span { start: 13, end: 19 }), Span { start: 13, end: 19 });
}

#[test]
fn lowers_scopes_trace_and_poison() {
    use steins_syntax::StmtKind;

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
    assert_eq!(top.stmts[2].invalidated.len(), 1);
    assert_eq!(top.stmts[2].invalidated[0].name, "w");

    // price() is a constant function: body is exactly `[Return(literal)]`.
    let price = tree.scopes().iter().find(|s| s.function_name.as_deref() == Some("price")).unwrap();
    assert!(!price.poisoned);
    assert_eq!(price.stmts.len(), 1);
    assert!(matches!(&price.stmts[0].kind, StmtKind::Return { value, .. } if value.is_literal()));
}

/// One `InvalidatedVar` flattened to borrowed parts, so a whole entry list
/// compares against a literal in one `assert_eq!`.
type EntryView<'a> = (&'a str, bool, Vec<(&'a str, u32)>);

#[test]
fn each_invalidated_name_carries_its_call_sites() {
    // ADR-0070 (issue #135): the syntax layer records WHERE each handed-over
    // variable went ON the name's own entry, and decides nothing. The entry
    // list stays the complete sound floor.
    let tree = SourceTree::parse("<?php $s = 'a'; trim($s);");
    let top = tree.scopes().iter().find(|s| s.function_name.is_none()).unwrap();
    let st = &top.stmts[1];
    assert_eq!(st.invalidated.len(), 1);
    let v = &st.invalidated[0];
    assert_eq!(v.name, "s");
    assert!(!v.opaque);
    assert_eq!(v.sites.len(), 1);
    assert_eq!(v.sites[0].0.raw, "trim");
    assert_eq!(v.sites[0].1, 0);

    // Positions are the argument indices, and a nested call is descended into
    // exactly as the name collection descends — they are one walk.
    let tree = SourceTree::parse("<?php $a = 1; $b = 2; f($a, g($b));");
    let top = tree.scopes().iter().find(|s| s.function_name.is_none()).unwrap();
    let st = &top.stmts[2];
    let entries: Vec<EntryView<'_>> = st
        .invalidated
        .iter()
        .map(|v| {
            (
                v.name.as_str(),
                v.opaque,
                v.sites.iter().map(|(c, p)| (c.raw.as_str(), *p)).collect(),
            )
        })
        .collect();
    assert_eq!(
        entries,
        vec![("a", false, vec![("f", 0)]), ("b", false, vec![("g", 0)])]
    );
}

#[test]
fn invalidated_names_are_the_bare_call_arguments_in_source_order() {
    // The name-set invariant: one entry per name, in first-occurrence source
    // order, and the names are exactly the statement's bare call arguments —
    // describable or not. `$b` occurs twice and keeps one entry with both
    // sites; `$d` goes to a method call and keeps an opaque entry; the method
    // receiver `$o` is not an argument and gets no entry at all.
    let tree = SourceTree::parse("<?php $x = f($b, $a, g($c), $b) . $o->m($d);");
    let top = tree.scopes().iter().find(|s| s.function_name.is_none()).unwrap();
    let st = top.stmts.last().unwrap();
    let names: Vec<&str> = st.invalidated.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, vec!["b", "a", "c", "d"]);
    let b = &st.invalidated[0];
    assert!(!b.opaque);
    assert_eq!(
        b.sites.iter().map(|(c, p)| (c.raw.as_str(), *p)).collect::<Vec<_>>(),
        vec![("f", 0), ("f", 3)]
    );
    let d = &st.invalidated[3];
    assert!(d.opaque, "a method-call argument is unprovable");
    assert!(d.sites.is_empty(), "an opaque entry carries no sites");
}

#[test]
fn an_unprovable_occurrence_marks_the_entry_opaque_with_no_sites() {
    // The explicit spelling of the old absence rule: ONE unprovable occurrence
    // anywhere in the statement makes the name's entry opaque, and an opaque
    // entry carries no sites — the provable `f($s)` site is discarded, not
    // kept beside the verdict.
    for (src, why) in [
        ("<?php $s = 'a'; $o = new C(); $x = f($s) . $o->m($s);", "method call"),
        ("<?php $s = 'a'; $o = new C(); $x = f($s) . $o?->m($s);", "nullsafe method call"),
        ("<?php $s = 'a'; $x = f($s) . C::m($s);", "static method call"),
        ("<?php $s = 'a'; $fn = 'trim'; $x = f($s) . $fn($s);", "dynamic callee"),
        ("<?php $s = 'a'; $x = f($s) . g(x: $s);", "named argument"),
        ("<?php $s = 'a'; $r = []; $x = f($s) . g($s, ...$r);", "spread"),
        (
            "<?php $s = 'a'; $c = function () use ($s) { return g($s); };",
            "closure-body occurrence",
        ),
        ("<?php $s = 'a'; $c = fn() => g($s);", "arrow-body occurrence"),
        ("<?php $s = 'a'; echo trim($s), $s = 'x';", "echo-embedded write"),
    ] {
        let tree = SourceTree::parse(src);
        let top = tree.scopes().iter().find(|s| s.function_name.is_none()).unwrap();
        let st = top.stmts.last().unwrap();
        let v = st
            .invalidated
            .iter()
            .find(|v| v.name == "s")
            .unwrap_or_else(|| panic!("{why}: still blanket-invalidated"));
        assert!(v.opaque, "{why}: one unprovable occurrence marks the entry opaque");
        assert!(v.sites.is_empty(), "{why}: an opaque entry carries no sites");
    }
    // The echo write is statement-scoped in the other direction too: the write
    // in the FIRST operand disqualifies a provable site in the second.
    let tree = SourceTree::parse("<?php $s = 'a'; echo $s = 'x', trim($s);");
    let top = tree.scopes().iter().find(|s| s.function_name.is_none()).unwrap();
    let st = top.stmts.last().unwrap();
    let v = st.invalidated.iter().find(|v| v.name == "s").unwrap();
    assert!(v.opaque && v.sites.is_empty(), "a site after the write verdict is refused");
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
fn class_likes_are_all_lowered_as_names() {
    // Interfaces (ADR-0033 Liskov), enums (ADR-0043), and — since ADR-0049 §5 —
    // traits all enter the class-like index, each marked by its kind flag. A trait
    // is name-only (no members) but present, so it occupies its FQN in the table.
    let src = "<?php\ninterface I {}\ntrait T {}\nenum E { case A; }\nclass C {}\n";
    let tree = SourceTree::parse(src);
    assert_eq!(tree.classes().len(), 4, "class, interface, enum, and trait are lowered");
    let c = tree.classes().iter().find(|d| d.name == "C").unwrap();
    assert!(!c.is_interface && !c.is_enum && !c.is_trait);
    let i = tree.classes().iter().find(|d| d.name == "I").unwrap();
    assert!(i.is_interface, "interface I is marked is_interface");
    let e = tree.classes().iter().find(|d| d.name == "E").unwrap();
    assert!(e.is_enum, "enum E is marked is_enum");
    assert!(e.is_final, "an enum is implicitly final");
    assert_eq!(e.enum_cases.len(), 1);
    assert_eq!(e.enum_cases[0].name, "A");
    let t = tree.classes().iter().find(|d| d.name == "T").unwrap();
    assert!(t.is_trait, "trait T is marked is_trait (ADR-0049 §5)");
    assert!(t.methods.is_empty(), "a trait is lowered name-only in S1");
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
    assert!(method_scopes[0].function_name.is_none());
}

#[test]
fn lowers_new_expression_as_class_fact_rvalue() {
    let src = "<?php $x = new Foo(\"abc\");";
    let tree = SourceTree::parse(src);
    let top = tree.scopes().iter().find(|s| s.function_name.is_none()).unwrap();
    let StmtKind::Assign { value, call, .. } = &top.stmts[0].kind else { panic!("assign") };
    assert!(matches!(value, ArgValue::New(c, _, _) if c.raw == "Foo"), "value is New(Foo)");
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
    // of the outer function (closure bodies are not scanned).
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
fn self_static_return_lower_parent_param_unlowered_intersection_lowers() {
    // Return-position `self`/`static` lower to a synthesized single-member
    // `Instance` of the enclosing class bound (ADR-0043 amendment — the LSB
    // minimum-bound check); `parent`/`self`/`static` in *parameter* position
    // stay unlowered (out of the amendment's return-only scope). An object
    // intersection (`A&B`) lowers to a single conjunctive `InstanceInter` member.
    let src = "<?php\nnamespace App;\nclass C {\n  function a(): self { return $this; }\n  function b(): static { return $this; }\n  function c(parent $p): void {}\n  function d(A&B $x): void {}\n}\n";
    let tree = SourceTree::parse(src);
    let c = tree.classes().iter().find(|d| d.name == "C").unwrap();
    let m = |name: &str| c.methods.iter().find(|m| m.name == name).unwrap();
    let enclosing =
        vec![TypeMember::Instance { fqn: "app\\c".into(), display: "App\\C".into() }];
    let a = m("a").ret.as_ref().expect("self return lowers to the enclosing-class bound");
    assert_eq!(a.members, enclosing, "self bound = enclosing class App\\C");
    assert!(!a.nullable);
    let b = m("b").ret.as_ref().expect("static return lowers to the enclosing-class bound");
    assert_eq!(b.members, enclosing, "static bound = enclosing class App\\C");
    assert!(!b.nullable);
    assert!(m("c").params[0].ty.is_none(), "parent param stays None");
    let d = m("d").params[0].ty.as_ref().expect("A&B lowers to an intersection member");
    assert_eq!(
        d.members,
        vec![TypeMember::InstanceInter(vec![
            ClassRef { fqn: "app\\a".into(), display: "App\\A".into() },
            ClassRef { fqn: "app\\b".into(), display: "App\\B".into() },
        ])],
        "A&B → one conjunctive InstanceInter member with both resolved classes"
    );
    assert!(!d.nullable);
    assert!(d.has_instance(), "an intersection member is object-bearing");
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

// ---- ADR-0049 §5: trait names into the class-like index --------------------

#[test]
fn trait_enters_the_class_like_index_as_a_name() {
    let tree = SourceTree::parse("<?php\nnamespace App;\ntrait Greet { public function hi(): void {} }\n");
    let t = class(&tree, "Greet");
    assert!(t.is_trait, "trait carries is_trait");
    assert!(!t.is_interface && !t.is_enum);
    assert_eq!(t.fqn, "app\\greet", "trait FQN is indexed lowercase");
    assert!(t.methods.is_empty(), "trait members are not lowered in S1");
    assert!(!t.conditional, "a top-level namespaced trait is unconditional");
}

// ---- ADR-0049 A2i: the conditional flag ------------------------------------

#[test]
fn top_level_and_namespaced_declarations_are_unconditional() {
    let tree = SourceTree::parse("<?php\nclass A {}\nnamespace N;\nclass B {}\n");
    assert!(!class(&tree, "A").conditional);
    assert!(!class(&tree, "B").conditional);
}

#[test]
fn a_class_guarded_by_class_exists_is_conditional() {
    let tree = SourceTree::parse(
        "<?php\nif (!class_exists('C')) {\n  class C {}\n}\n",
    );
    assert!(class(&tree, "C").conditional, "a class inside `if` is conditional");
}

#[test]
fn declarations_inside_a_function_body_are_conditional() {
    let tree = SourceTree::parse(
        "<?php\nfunction boot(): void {\n  class Inner {}\n  interface I {}\n  enum E {}\n  trait T {}\n}\n",
    );
    for name in ["Inner", "I", "E", "T"] {
        assert!(class(&tree, name).conditional, "{name} inside a function body is conditional");
    }
}

// ---- ADR-0049 §2: class_alias edges + non-literal dam sites -----------------

use steins_syntax::ClassAliasEdge;

#[test]
fn literal_class_alias_lowers_to_an_edge() {
    let tree = SourceTree::parse("<?php\nclass_alias('App\\\\Legacy', 'App\\\\Modern');\n");
    let edges = tree.class_alias_edges();
    assert_eq!(edges.len(), 1);
    let ClassAliasEdge { alias_fqn, target_fqn, .. } = &edges[0];
    // arg 0 = existing target, arg 1 = new alias name; both lowercase FQN.
    assert_eq!(target_fqn, "app\\legacy");
    assert_eq!(alias_fqn, "app\\modern");
    // A literal alias is NOT a dynamism/dam site.
    assert!(tree.dynamism_sites().is_empty());
}

#[test]
fn fully_qualified_class_alias_is_recognized() {
    let tree = SourceTree::parse("<?php\n\\class_alias('A', 'B');\n");
    assert_eq!(tree.class_alias_edges().len(), 1);
    assert!(tree.dynamism_sites().is_empty());
}

#[test]
fn non_literal_class_alias_is_a_dynamism_site() {
    // A computed target/alias cannot mint an edge — it is a runtime name mint.
    let tree = SourceTree::parse("<?php\nclass_alias($src, 'B');\n");
    assert!(tree.class_alias_edges().is_empty());
    let sites = tree.dynamism_sites();
    assert_eq!(sites.len(), 1);
    assert!(matches!(sites[0].kind, DynamismKind::ClassAlias));
}

// ---- issue #36: `X::class` is compile-time, so it mints an edge, not a dam ----

/// The single edge a source lowers to, as `(target_fqn, alias_fqn)`, asserting the
/// call left no dam site behind. Panics unless there is exactly one edge.
fn only_edge(tree: &SourceTree) -> (String, String) {
    assert!(tree.dynamism_sites().is_empty(), "unexpected dam site: {:?}", tree.dynamism_sites());
    let edges = tree.class_alias_edges();
    assert_eq!(edges.len(), 1, "{edges:?}");
    (edges[0].target_fqn.clone(), edges[0].alias_fqn.clone())
}

/// Whether a source dams (a `ClassAlias` dynamism site, no edge).
fn alias_dams(src: &str) -> bool {
    let tree = SourceTree::parse(src);
    tree.class_alias_edges().is_empty()
        && tree.dynamism_sites().iter().any(|s| matches!(s.kind, DynamismKind::ClassAlias))
}

#[test]
fn class_const_target_lowers_to_an_edge() {
    // The issue's minimal repro: `X::class` is resolved by the compiler, so this is
    // an alias edge — not "a runtime class-name mint".
    let tree = SourceTree::parse("<?php\nclass Thing {}\nclass_alias(Thing::class, 'Legacy_Thing');\n");
    assert_eq!(only_edge(&tree), ("thing".to_owned(), "legacy_thing".to_owned()));
}

#[test]
fn class_const_in_either_or_both_positions_lowers_to_an_edge() {
    // The alias position too — `class_alias('A', B::class)` is equally compile-time.
    let tree = SourceTree::parse("<?php\nclass_alias('A', B::class);\n");
    assert_eq!(only_edge(&tree), ("a".to_owned(), "b".to_owned()));
    let tree = SourceTree::parse("<?php\nclass_alias(A::class, B::class);\n");
    assert_eq!(only_edge(&tree), ("a".to_owned(), "b".to_owned()));
}

#[test]
fn class_const_target_resolves_through_the_namespace_context() {
    // `X::class` is subject to ordinary class-name resolution, unlike a literal
    // (which is a runtime FQN taken as written). The enclosing namespace applies…
    let tree = SourceTree::parse("<?php\nnamespace App;\nclass Thing {}\nclass_alias(Thing::class, 'Legacy');\n");
    assert_eq!(only_edge(&tree), ("app\\thing".to_owned(), "legacy".to_owned()));
    // …a fully-qualified spelling escapes it…
    let tree = SourceTree::parse("<?php\nnamespace App;\nclass_alias(\\Other\\Thing::class, 'Legacy');\n");
    assert_eq!(only_edge(&tree), ("other\\thing".to_owned(), "legacy".to_owned()));
    // …and `namespace\X` resolves against the namespace without doubling it.
    let tree = SourceTree::parse("<?php\nnamespace App;\nclass_alias(namespace\\Thing::class, 'Legacy');\n");
    assert_eq!(only_edge(&tree), ("app\\thing".to_owned(), "legacy".to_owned()));
}

#[test]
fn class_const_target_resolves_through_use_imports() {
    // Plain, aliased, and GROUPED `use` — the grouped form is the shape that
    // previously mis-resolved to a same-named class in the fallback namespace.
    let tree = SourceTree::parse("<?php\nuse Vendor\\Pkg\\Thing;\nclass_alias(Thing::class, 'Legacy');\n");
    assert_eq!(only_edge(&tree), ("vendor\\pkg\\thing".to_owned(), "legacy".to_owned()));
    let tree = SourceTree::parse("<?php\nuse Vendor\\Pkg\\Thing as T;\nclass_alias(T::class, 'Legacy');\n");
    assert_eq!(only_edge(&tree), ("vendor\\pkg\\thing".to_owned(), "legacy".to_owned()));
    let tree = SourceTree::parse("<?php\nuse Vendor\\{Pkg\\Thing, Other};\nclass_alias(Thing::class, 'Legacy');\n");
    assert_eq!(only_edge(&tree), ("vendor\\pkg\\thing".to_owned(), "legacy".to_owned()));
    // A qualified reference imports on its FIRST segment only.
    let tree = SourceTree::parse("<?php\nuse Vendor\\Pkg;\nclass_alias(Pkg\\Thing::class, 'Legacy');\n");
    assert_eq!(only_edge(&tree), ("vendor\\pkg\\thing".to_owned(), "legacy".to_owned()));
}

#[test]
fn class_const_on_an_unresolvable_name_still_lowers_to_an_edge() {
    // `X::class` neither autoloads nor requires `X` to exist (PHP 8.0+), so the
    // lowering is unconditional. Whether the alias *backs an existence claim* is the
    // index fold's call — an edge to an absent target mints nothing there — so a
    // name the index cannot resolve costs nothing and must not dam.
    let tree = SourceTree::parse("<?php\nclass_alias(NeverDeclared::class, 'Legacy');\n");
    assert_eq!(only_edge(&tree), ("neverdeclared".to_owned(), "legacy".to_owned()));
}

#[test]
fn self_static_and_parent_class_still_dam() {
    // `static::class` is late-static-bound — unknowable at the site. `self::class` /
    // `parent::class` need a lexical-class context this file-wide walk does not
    // carry. All three keep damming, the sound direction.
    for kw in ["self", "static", "parent"] {
        let src = format!(
            "<?php\nclass C extends P {{ public function f(): void {{ class_alias({kw}::class, 'Legacy'); }} }}\n"
        );
        assert!(alias_dams(&src), "{kw}::class must dam");
    }
}

#[test]
fn genuinely_runtime_alias_names_still_dam() {
    // The widening stops at `::class`. Everything whose name is minted at run time —
    // a variable, a concatenation (even one touching `::class`), a call, a constant,
    // a dynamic class expression, a dynamic constant name — keeps damming.
    for src in [
        "<?php\nclass_alias($src, 'B');\n",
        "<?php\nclass_alias('A', $dst);\n",
        "<?php\nclass_alias(Thing::class . 'Suffix', 'B');\n",
        "<?php\nclass_alias('Prefix' . Thing::class, 'B');\n",
        "<?php\nclass_alias(get_class($o), 'B');\n",
        "<?php\nclass_alias(TARGET_CLASS, 'B');\n",
        "<?php\nclass_alias($cls::class, 'B');\n",
        "<?php\nclass_alias(Thing::{$k}, 'B');\n",
        "<?php\nclass_alias(Thing::NAME, 'B');\n",
        "<?php\nclass_alias(...$args);\n",
        "<?php\nclass_alias(alias: 'B', class: A::class);\n",
    ] {
        assert!(alias_dams(src), "must dam: {src}");
    }
}

// ---- issue #30: the opaque-construct inventory behind the poison flag -------

use steins_syntax::{OpaqueConstruct, ReflectionKind};

/// The give-up-list constructs of the top-level scope, in source order.
fn top_opaque(src: &str) -> Vec<OpaqueConstruct> {
    let tree = SourceTree::parse(src);
    let top = tree.scopes().iter().find(|s| s.function_name.is_none()).unwrap();
    top.opaque.iter().map(|s| s.construct).collect()
}

#[test]
fn every_poison_marker_names_itself_in_the_inventory() {
    // The same sources as `poison_markers_are_detected`, now asserting WHICH
    // construct was recognized — the inventory is the predicate's own vocabulary.
    for (src, want) in [
        ("<?php $r = &$w; width($w);", OpaqueConstruct::ReferenceAssign),
        ("<?php extract($d); width($w);", OpaqueConstruct::Extract),
        ("<?php compact('w'); width($w);", OpaqueConstruct::Compact),
        ("<?php global $w; width($w);", OpaqueConstruct::Global),
        ("<?php static $w = 1; width($w);", OpaqueConstruct::StaticVar),
        ("<?php $$w = 1; width($w);", OpaqueConstruct::VariableVariable),
        ("<?php $f = function () use (&$w) {}; width($w);", OpaqueConstruct::ByRefCapture),
        ("<?php eval($c);", OpaqueConstruct::Eval),
        ("<?php include $p;", OpaqueConstruct::Include),
        ("<?php include_once $p;", OpaqueConstruct::Include),
        ("<?php require $p;", OpaqueConstruct::Include),
        ("<?php require_once $p;", OpaqueConstruct::Include),
    ] {
        assert_eq!(top_opaque(src), vec![want], "`{src}`");
    }
}

#[test]
fn the_poison_flag_is_the_inventory_being_non_empty() {
    // The anti-drift invariant, asserted over every scope of a mixed file: the flag
    // and the inventory come from one walk, so they cannot disagree.
    let tree = SourceTree::parse(
        "<?php\n\
         function clean(int $a): int { return $a; }\n\
         function dirty(array $r): void { extract($r); }\n\
         $f = function () use (&$t) {};\n\
         $g = fn($n) => eval($n);\n\
         class C { public function m(): void { global $x; } }\n",
    );
    assert!(tree.scopes().len() >= 5, "scopes: {}", tree.scopes().len());
    for scope in tree.scopes() {
        assert_eq!(scope.poisoned, !scope.opaque.is_empty(), "{:?}", scope.owner);
    }
    let poisoned = tree.scopes().iter().filter(|s| s.poisoned).count();
    // The top level is poisoned too: the by-ref `use (&$t)` capture aliases one of
    // ITS locals (ADR-0033), so the fact lands on both sides of the capture.
    assert_eq!(poisoned, 5, "top level + dirty + the closure + the arrow fn + C::m");
}

#[test]
fn a_clean_scope_carries_no_sites() {
    let tree = SourceTree::parse("<?php\nfunction f(int $x): int { return $x; }\nf(1);\n");
    assert!(tree.scopes().iter().all(|s| s.opaque.is_empty() && !s.poisoned));
}

#[test]
fn a_byref_capture_is_a_site_on_both_scopes() {
    // One aliasing fact, two scopes (ADR-0033): the enclosing scope and the
    // closure's own. Both must name it, or the inventory under-reports the closure.
    let tree = SourceTree::parse("<?php\n$t = 0;\n$f = function () use (&$t) { $t++; };\n");
    let sites: Vec<OpaqueConstruct> =
        tree.scopes().iter().flat_map(|s| s.opaque.iter().map(|o| o.construct)).collect();
    assert_eq!(sites, vec![OpaqueConstruct::ByRefCapture, OpaqueConstruct::ByRefCapture]);
}

#[test]
fn a_nested_scopes_construct_belongs_to_that_scope_only() {
    // A function body's `extract` poisons the function, never the top level.
    let tree = SourceTree::parse("<?php\nfunction f(array $r): void { extract($r); }\n$w = 1;\n");
    assert!(top_opaque("<?php\nfunction f(array $r): void { extract($r); }\n$w = 1;\n").is_empty());
    let f = tree.scopes().iter().find(|s| s.function_name.as_deref() == Some("f")).unwrap();
    assert_eq!(f.opaque.iter().map(|o| o.construct).collect::<Vec<_>>(), vec![OpaqueConstruct::Extract]);
}

#[test]
fn every_construct_kind_has_a_label() {
    // `ALL` is hand-maintained beside an exhaustive `label` match; this pins the
    // pair together so a new variant cannot silently vanish from the report.
    assert_eq!(OpaqueConstruct::ALL.len(), 9);
    let mut labels: Vec<&str> = OpaqueConstruct::ALL.iter().map(|c| c.label()).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), OpaqueConstruct::ALL.len(), "labels must be distinct");
}

// ---- issue #30: reflection-driven invocation (report-only, an admitted guess) ---

fn reflection_kinds(src: &str) -> Vec<ReflectionKind> {
    SourceTree::parse(src).reflection_sites().iter().map(|s| s.kind).collect()
}

#[test]
fn invoke_and_new_instance_shapes_are_inventoried() {
    for (src, want) in [
        ("<?php $m->invoke($o);", ReflectionKind::Invoke),
        ("<?php $m->invokeArgs($o, $a);", ReflectionKind::Invoke),
        ("<?php $c->newInstance();", ReflectionKind::NewInstance),
        ("<?php $c->newInstanceArgs($a);", ReflectionKind::NewInstance),
        ("<?php $c->newInstanceWithoutConstructor();", ReflectionKind::NewInstance),
        ("<?php $c?->invoke($o);", ReflectionKind::Invoke),
    ] {
        assert_eq!(reflection_kinds(src), vec![want], "`{src}`");
    }
    // `__invoke` is not the `invoke` prefix, and an unrelated method is not a site.
    assert!(reflection_kinds("<?php $c->__invoke(); $o->render();").is_empty());
}

#[test]
fn closure_bind_counts_only_a_computed_scope() {
    assert_eq!(
        reflection_kinds("<?php \\Closure::bind($fn, $o, $scope);"),
        vec![ReflectionKind::ClosureBindComputedScope]
    );
    // A statically named scope resolves — nothing is hidden from the analyzer.
    for literal in [
        "<?php Closure::bind($fn, $o, 'App\\\\Legacy');",
        "<?php Closure::bind($fn, $o, Legacy::class);",
        "<?php Closure::bind($fn, $o, null);",
        "<?php Closure::bind($fn, $o);",
        "<?php Other::bind($fn, $o, $scope);",
    ] {
        assert!(reflection_kinds(literal).is_empty(), "`{literal}`");
    }
}

#[test]
fn func_get_args_counts_only_under_a_typed_signature() {
    // A parameter hint is a claim about the argument list; so is a return hint.
    for src in [
        "<?php function f(int $a) { return func_get_args(); }",
        "<?php function f($a): array { return func_get_args(); }",
        "<?php class C { public function m(int $a) { return func_get_args(); } }",
        "<?php $f = function (int $a) { return func_get_args(); };",
        "<?php $f = fn(int $a) => func_get_args();",
    ] {
        assert_eq!(
            reflection_kinds(src),
            vec![ReflectionKind::FuncGetArgsInTypedSignature],
            "`{src}`"
        );
    }
    // No hint anywhere: the signature claims nothing, so nothing is contradicted.
    for src in [
        "<?php function f($a) { return func_get_args(); }",
        "<?php func_get_args();",
        // The nearest enclosing function-like decides: an untyped closure inside a
        // typed method is untyped.
        "<?php class C { public function m(int $a): array { $f = function () { return func_get_args(); }; return []; } }",
    ] {
        assert!(reflection_kinds(src).is_empty(), "`{src}`");
    }
}

#[test]
fn reflection_sites_poison_nothing() {
    // The inventory is report-only: recognizing a reflective call must not change a
    // single analysis decision.
    let tree = SourceTree::parse("<?php\nfunction f(\\ReflectionMethod $m, object $o): mixed {\n  return $m->invoke($o);\n}\n");
    assert_eq!(tree.reflection_sites().len(), 1);
    assert!(tree.scopes().iter().all(|s| !s.poisoned));
    assert!(tree.dynamism_sites().is_empty());
}

/// A method name in a multibyte script must not panic the reflection-site
/// lowering (issue #30's inventory): `name[..11]` on a Japanese method name is
/// not a char boundary. Found by running the analyzer over ec-cube, whose
/// domain methods are named in Japanese — real code, not a fuzzer artifact.
#[test]
fn multibyte_method_names_do_not_panic_reflection_lowering() {
    let src = "<?php\n\
        $q->キャンセル処理を実行する();\n\
        $q->invoke何か();\n\
        $q->newInstance生成();\n\
        $q->invokeHandler();\n";
    let tree = steins_syntax::SourceTree::parse(src);
    // The ASCII-prefixed calls still classify; the multibyte-led one is simply
    // not a reflection site.
    assert!(tree.parse_errors().is_empty());
}
