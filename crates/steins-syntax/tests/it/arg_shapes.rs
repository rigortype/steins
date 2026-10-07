//! What the effect scan shows a call argument holds (issue #856): the
//! `ArgShape` a call origin carries per positional argument, by the
//! argument's form or by a flow-insensitive summary of the frame's writes.

use steins_syntax::{
    ArgShape, EffectOrigin, FloatEvidence, SourceTree, Stored, derive_effect_origins,
};

/// The shapes a call to `g` carries, if `origin` is one.
fn g_shapes(origin: &EffectOrigin) -> Option<Vec<ArgShape>> {
    match origin {
        EffectOrigin::Call { name, arg_shapes, .. } if name.simple() == "g" => arg_shapes.clone(),
        // A string literal is a callback candidate, so the call lands here.
        EffectOrigin::HigherOrder { callee, arg_shapes, .. } if callee.simple() == "g" => {
            Some(arg_shapes.clone())
        }
        _ => None,
    }
}

/// The shapes of the call to `g` in `f`'s body.
fn shapes(signature: &str, body: &str) -> Vec<ArgShape> {
    let src = format!("<?php\nfunction f({signature}) {{ {body} }}\n");
    let tree = SourceTree::parse(&src);
    let f = tree.functions().iter().find(|f| f.name == "f").expect("f").clone();
    derive_effect_origins(&f.sites)
        .iter()
        .find_map(g_shapes)
        .unwrap_or_else(|| panic!("no positional call to g in {:?}", derive_effect_origins(&f.sites)))
}

fn param(name: &str, stores: Stored) -> ArgShape {
    ArgShape::Param { name: name.to_owned(), stores }
}

fn local(name: &str, stores: Stored) -> ArgShape {
    ArgShape::Local { name: name.to_owned(), stores }
}

#[test]
fn an_expression_qualifies_by_its_form() {
    use ArgShape::{Array, ObjectFree, Unknown};
    let args = "1, 'a', \"x{$v}\", 'a' . $v, $v === 1, !$v, (string) $v, isset($v), [1, ['k' => 2]]";
    assert_eq!(shapes("$v", &format!("g({args});")), vec![ObjectFree; 9]);
    let eol = ArgShape::GlobalConst(steins_syntax::NameRef {
        raw: "PHP_EOL".to_owned(),
        kind: steins_syntax::RefKind::Unqualified,
        offset: 0,
    });
    assert_eq!(shapes("$v", "g([$v], (array) $v, $v + 1, PHP_EOL, $v ? 'a' : 'b');"), [
        Array, Array, Unknown, eol, ObjectFree
    ]);
}

/// A class name is a string (`Foo::class`, `static::class`, `$o::class`); any other class
/// constant or enum case is a shape of its own, and a `match` or a `throw` is object-free when
/// its arms are (issue #868).
#[test]
fn a_class_constant_is_its_own_shape_and_a_class_name_holds_no_object() {
    use ArgShape::{ClassConst, ObjectFree, Unknown};
    let args = "Foo::class, static::class, $v::class, self::class";
    assert_eq!(shapes("$v", &format!("g({args});")), vec![ObjectFree; 4]);
    let consts = "g(Foo::BAR, self::NAME, Suit::Hearts, static::X);";
    assert_eq!(shapes("$v", consts), vec![ClassConst; 4]);
    assert_eq!(shapes("$v", "g(Foo::{$v}, $v::BAR);"), [Unknown, ClassConst]);
    let arms =
        "g(match ($v) { 1 => 'a', default => null }, match ($v) { 1 => $v, default => 'b' });";
    assert_eq!(shapes("$v", arms), [ObjectFree, Unknown]);
}

/// A call result carries its callee (issue #877): the syntax crate cannot ask the
/// catalog or the project what it returns, so the engine does. The receiver is
/// spelled as a method-call site's is, and a callee the scan cannot name is unknown.
#[test]
fn a_call_result_names_its_callee() {
    use steins_syntax::{EffectRecv, NameRef, RefKind};
    let call = |name: &str| ArgShape::Call(NameRef { raw: name.to_owned(), kind: RefKind::Unqualified, offset: 0 });
    let method = |receiver, name: &str| ArgShape::MethodCall { receiver, method: name.to_owned() };
    let class = |name: &str| EffectRecv::ClassName(NameRef { raw: name.to_owned(), kind: RefKind::Unqualified, offset: 0 });
    assert_eq!(shapes("$v", "g(h(), h($v));"), [call("h"), call("h")]);
    assert_eq!(shapes("$v", "g(strlen($v));"), [call("strlen")]);
    assert_eq!(shapes("$v", "g($this->m(), self::m(), parent::m(), Foo::m());"), [
        method(EffectRecv::This, "m"),
        method(EffectRecv::SelfKw, "m"),
        method(EffectRecv::Parent, "m"),
        method(class("Foo"), "m"),
    ]);
    assert_eq!(shapes("$v", "g((new Foo())->m());"), [method(class("Foo"), "m")]);
    // A parameter nothing writes names its declared type; a written one, a
    // dynamic callee, a dynamic method name and a null-safe call name nothing.
    assert_eq!(shapes("Foo $r", "g($r->m());"), [method(EffectRecv::Var("r".to_owned()), "m")]);
    assert_eq!(shapes("Foo $r", "$r = h(); g($r->m());"), [ArgShape::Unknown]);
    assert_eq!(shapes("$f", "g($f());"), [ArgShape::Unknown]);
    assert_eq!(shapes("Foo $r, $n", "g($r->$n());"), [ArgShape::Unknown]);
    assert_eq!(shapes("?Foo $r", "g($r?->m());"), [ArgShape::Unknown]);
    assert_eq!(shapes("$v", "g(static::m());"), [ArgShape::Unknown]);
}

#[test]
fn a_parameter_meets_every_write_the_frame_makes() {
    assert_eq!(shapes("string $s", "g($s);"), [param("s", Stored::ObjectFree)]);
    assert_eq!(shapes("int|string $c", "g($c); $c = 0;"), [param("c", Stored::ObjectFree)]);
    assert_eq!(shapes("string $m", "$m .= h(); g($m);"), [param("m", Stored::ObjectFree)]);
    assert_eq!(shapes("array $a", "$a[] = h(); g($a);"), [param("a", Stored::Array)]);
    assert_eq!(shapes("string $s", "$s = h(); g($s);"), [ArgShape::Unknown]);
    assert_eq!(shapes("string $s", "foreach (h() as $s) {} g($s);"), [ArgShape::Unknown]);
    assert_eq!(shapes("string $s", "try {} catch (E $s) {} g($s);"), [ArgShape::Unknown]);
    // A by-ref or variadic parameter is bound by the caller.
    assert_eq!(shapes("string &$s", "g($s);"), [ArgShape::Unknown]);
    assert_eq!(shapes("string ...$s", "g($s);"), [ArgShape::Unknown]);
}

#[test]
fn a_local_starts_unset_and_meets_its_writes() {
    assert_eq!(shapes("", "$x = 'a'; g($x);"), [local("x", Stored::ObjectFree)]);
    assert_eq!(shapes("", "$x = []; $x[] = 'a'; g($x);"), [local("x", Stored::ObjectFree)]);
    assert_eq!(shapes("", "$x = []; $x[] = h(); g($x);"), [local("x", Stored::Array)]);
    assert_eq!(shapes("", "g($never);"), [local("never", Stored::ObjectFree)]);
    assert_eq!(shapes("", "[$a, $b] = h(); g($a);"), [ArgShape::Unknown]);
    assert_eq!(shapes("", "[$a, $b] = ['x', 'y']; g($a);"), [local("a", Stored::ObjectFree)]);
}

/// A variable handed to a call the effects pass cannot check against its
/// callee's parameters counts as written there; a bare one in a named call or
/// a resolvable method call is left for the effects pass.
#[test]
fn an_argument_no_callee_accounts_for_counts_as_a_write() {
    assert_eq!(shapes("string $s, $o", "$o->m($s); g($s);"), [ArgShape::Unknown]);
    assert_eq!(shapes("string $s, $o", "h($s['k']); g($s);"), [ArgShape::Unknown]);
    assert_eq!(shapes("string $s", "h(...[$s]); g($s);"), [param("s", Stored::ObjectFree)]);
    assert_eq!(shapes("string $s", "h(x: $s); g($s);"), [ArgShape::Unknown]);
    assert_eq!(shapes("string $s", "h($s); Foo::m($s); new Bar($s); g($s);"), [
        param("s", Stored::ObjectFree)
    ]);
}

#[test]
fn an_aliasing_or_importing_frame_shows_no_variable() {
    assert_eq!(shapes("string $s", "global $g; g($s);"), [ArgShape::Unknown]);
    assert_eq!(shapes("string $s", "static $n = 0; g($s);"), [ArgShape::Unknown]);
    assert_eq!(shapes("string $s", "$r = &$s; g($s);"), [ArgShape::Unknown]);
    // A closure's `use` capture is bound by its parent; an arrow function
    // captures every free variable.
    let src = "<?php\nfunction f(string $s) {\n\
               $c = function (string $p) use ($s) { g($s, $p); };\n\
               $a = fn (string $p) => g($x, $p);\n}\n";
    let tree = SourceTree::parse(src);
    let calls: Vec<Vec<ArgShape>> = tree
        .scopes()
        .iter()
        .flat_map(|s| derive_effect_origins(&s.sites))
        .filter_map(|o| g_shapes(&o))
        .collect();
    assert_eq!(calls, [
        vec![ArgShape::Unknown, param("p", Stored::ObjectFree)],
        vec![ArgShape::Unknown, param("p", Stored::ObjectFree)],
    ]);
}

#[test]
fn a_this_property_and_a_resolvable_method_call_carry_their_shapes() {
    let src = "<?php\nclass C {\n private string $p = '';\n\
               public function m(string $s) {\n\
               g($this->p); $this->n($s, 1); parent::__construct($s); $o = new D($s);\n\
               }\n}\n";
    let tree = SourceTree::parse(src);
    let m = &tree.classes()[0].methods[0];
    let mut seen = Vec::new();
    for origin in &derive_effect_origins(&m.sites) {
        match origin {
            EffectOrigin::Call { arg_shapes, .. } => seen.push(("call", arg_shapes.clone())),
            EffectOrigin::MethodCall { method, arg_shapes, .. } => {
                seen.push((if method == "n" { "n" } else { "ctor" }, arg_shapes.clone()));
            }
            EffectOrigin::New { arg_shapes, .. } => seen.push(("new", arg_shapes.clone())),
            _ => {}
        }
    }
    let s = param("s", Stored::ObjectFree);
    assert_eq!(seen, [
        ("call", Some(vec![ArgShape::ThisProperty("p".to_owned())])),
        ("n", Some(vec![s.clone(), ArgShape::ObjectFree])),
        ("ctor", Some(vec![s.clone()])),
        ("new", Some(vec![s])),
    ]);
}

/// The evidence a `sprintf` call at the end of `f`'s body carries about whether its arguments
/// are floats (`ConstArgs::float_evidence`, ADR-0101 §3.8).
fn evidence(signature: &str, body: &str) -> Vec<(u8, FloatEvidence)> {
    let src = format!("<?php\nfunction f({signature}) {{ {body} }}\n");
    let tree = SourceTree::parse(&src);
    let f = tree.functions().iter().find(|f| f.name == "f").expect("f").clone();
    derive_effect_origins(&f.sites)
        .iter()
        .rev()
        .find_map(|origin| match origin {
            EffectOrigin::Call { name, const_args, .. }
            | EffectOrigin::HigherOrder { callee: name, const_args, .. }
                if matches!(name.simple(), "sprintf" | "printf" | "Sprintf" | "g") =>
            {
                Some(const_args.float_evidence.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no printf-family call in {:?}", derive_effect_origins(&f.sites)))
}

fn shape_of(shape: ArgShape) -> FloatEvidence {
    FloatEvidence::Shape { shape, unwritten: false, writes: Vec::new() }
}

/// A form that is no float is `NoFloat`, one that is a float is `Float`; the format at
/// position 0 is never recorded, and arithmetic is withheld.
#[test]
fn a_printf_argument_is_a_float_or_not_by_its_form() {
    use FloatEvidence::{Float, NoFloat};
    let args = "1, 'a', true, null, \"x{$v}\", 'a' . $v, $v === 1, !$v, (string) $v, (int) $v, [1], \
                __LINE__, -1";
    let shown: Vec<(u8, FloatEvidence)> = evidence("$v", &format!("sprintf('%s', {args});"));
    assert_eq!(shown.len(), 13);
    for (i, (position, ev)) in shown.iter().enumerate() {
        assert_eq!(usize::from(*position), i + 1);
        assert_eq!(*ev, NoFloat, "argument {position}");
    }
    // An integer literal is an `int` only while it fits one; past it, it is a float.
    assert_eq!(evidence("$v", "sprintf('%s', 9223372036854775807);"), [(1, NoFloat)]);
    for wide in ["9223372036854775808", "0xFFFFFFFFFFFFFFFF", "99999999999999999999", "-9223372036854775808"] {
        assert_eq!(evidence("$v", &format!("sprintf('%s', {wide});")), [(1, Float)], "{wide}");
    }
    for float in ["1.5", "(float) $v", "(double) $v", "-1.5", "(1.5)", "@(float) $v", "1e3"] {
        assert_eq!(evidence("$v", &format!("sprintf('%s', {float});")), [(1, Float)], "{float}");
    }
    for withheld in ["$v + 1", "$v * 2", "1 / 3", "-$v"] {
        assert_eq!(evidence("$v", &format!("sprintf('%s', {withheld});")), [], "{withheld}");
    }
    // The family is `sprintf` and `printf`; another name records nothing.
    assert_eq!(evidence("$v", "printf('%s', 'a');"), [(1, NoFloat)]);
    assert_eq!(evidence("$v", "g('%s', 'a');"), []);
    // A named or spread list defeats positional mapping.
    assert_eq!(evidence("$v", "sprintf(...$v);"), []);
    assert_eq!(evidence("$v", "sprintf(format: '%s', values: 'a');"), []);
}

/// A ternary or `??` is the join of its branches, each read as a top-level value.
#[test]
fn a_conditional_is_the_evidence_of_its_branches() {
    use FloatEvidence::{Float, NoFloat, OneOf};
    assert_eq!(evidence("$v", "sprintf('%s', $v ? 'a' : 1);"), [(1, NoFloat)]);
    assert_eq!(evidence("$v", "sprintf('%s', $v ? 'a' : 1.5);"), [(1, OneOf(vec![NoFloat, Float]))]);
    assert_eq!(evidence("$v", "sprintf('%s', 'a' ?: 'b');"), [(1, NoFloat)]);
    assert_eq!(evidence("$v", "sprintf('%s', 'b' ?? 'a');"), [(1, NoFloat)]);
    // A branch that shows nothing withholds the whole value.
    assert_eq!(evidence("$v", "sprintf('%s', $v ? 'a' : $v + 1);"), []);
    assert_eq!(evidence("$v", "sprintf('%s', $v ?: 'a');"), [(
        1,
        OneOf(vec![FloatEvidence::Shape {
            shape: ArgShape::Param { name: "v".to_owned(), stores: Stored::ObjectFree },
            unwritten: true,
            writes: Vec::new()
        }, NoFloat])
    )]);
    // The branches nest.
    assert_eq!(
        evidence("$v", "sprintf('%s', $v ? 'a' : ($v ? 1 : 2));"),
        [(1, NoFloat)],
    );
}

/// A bare variable carries its shape while no write of the frame may leave a float in it that
/// the scan cannot name; the parameter's declared type and the by-reference question are the
/// engine's.
#[test]
fn a_printf_variable_carries_its_shape_until_a_write_may_leave_a_float() {
    use FloatEvidence::{Float, NoFloat};
    let p = |name: &str, unwritten: bool, writes: Vec<FloatEvidence>| {
        FloatEvidence::Shape {
            shape: ArgShape::Param { name: name.to_owned(), stores: Stored::ObjectFree },
            unwritten,
            writes,
        }
    };
    let l = |name: &str, writes: Vec<FloatEvidence>| FloatEvidence::Shape {
        shape: ArgShape::Local { name: name.to_owned(), stores: Stored::ObjectFree },
        unwritten: false,
        writes,
    };
    assert_eq!(evidence("string $s", "sprintf('%s', $s);"), [(1, p("s", true, vec![]))]);
    assert_eq!(
        evidence("", "$a = 'x'; $b = 1; $c = $a . 'y'; sprintf('%s%s%s', $a, $b, $c);"),
        [(1, l("a", vec![])), (2, l("b", vec![])), (3, l("c", vec![]))]
    );
    // A parameter nothing writes is `unwritten`; a no-float write keeps it no float.
    assert_eq!(
        evidence("int $i", "$i = (string) $i; sprintf('%s', $i);"),
        [(1, p("i", false, vec![]))]
    );
    assert_eq!(evidence("$s", "$s .= 'x'; sprintf('%s', $s);"), [(1, p("s", false, vec![]))]);
    assert_eq!(evidence("$s", "unset($s); sprintf('%s', $s);"), [(1, p("s", false, vec![]))]);
    assert_eq!(evidence("", "$a = []; $a[] = 1.5; sprintf('%s', $a);"), [(1, l("a", vec![]))]);
    // A write the scan can name but that is no plain no-float form is carried, and the engine
    // reads what it is: a float, a call result, a constant, a ternary.
    assert_eq!(evidence("$v", "$x = 1.5; sprintf('%s', $x);"), [(1, l("x", vec![Float]))]);
    assert_eq!(
        evidence("$v", "$x = 'a'; $x = $v ? 'b' : 'c'; sprintf('%s', $x);"),
        [(1, l("x", vec![]))],
    );
    let call = |name: &str| {
        shape_of(ArgShape::Call(steins_syntax::NameRef {
            raw: name.to_owned(),
            kind: steins_syntax::RefKind::Unqualified,
            offset: 0,
        }))
    };
    let got = evidence("$v", "$x = h(1); $x = strlen('a'); sprintf('%s', $x);");
    assert_eq!(got.len(), 1);
    let FloatEvidence::Shape { writes, .. } = &got[0].1 else { panic!("{got:?}") };
    assert_eq!(writes.len(), 2);
    assert!(matches!(&writes[0], FloatEvidence::Shape { shape: ArgShape::Call(n), .. } if n.simple() == "h"));
    assert!(matches!(&writes[1], FloatEvidence::Shape { shape: ArgShape::Call(n), .. } if n.simple() == "strlen"));
    let _ = (call("h"), NoFloat);
    // Each of these may leave a float the scan cannot name: arithmetic (integers overflow), an
    // increment (the greatest integer overflows), a loop or `catch` binding, a destructuring
    // target, a by-ref argument, a reference, an unnamed value.
    for write in [
        "$x = 1; $x += 1;",
        "$x = 1; $x++;",
        "$x = 1; --$x;",
        "foreach ($v as $x) {}",
        "foreach ($v as $k => $x) {}",
        "[$x] = $v;",
        "list($x) = $v;",
        "try {} catch (E $x) {}",
        "$x = $y = 1.5;",
        "h($x[0]);",
        "$x = 'a'; $x *= 2;",
        "$x = $v + 1;",
        "$x = $v;",
        "$x = $v ? 1 : $v;",
    ] {
        assert_eq!(evidence("$v", &format!("{write} sprintf('%s', $x);")), [], "{write}");
    }
    // A variable the frame imports or aliases shows nothing, and neither does `$this`.
    assert_eq!(evidence("&$r", "sprintf('%s', $r);"), []);
    assert_eq!(evidence("...$r", "sprintf('%s', $r);"), []);
    assert_eq!(evidence("$v", "global $g; sprintf('%s', $g);"), []);
    assert_eq!(evidence("$v", "$r = &$v; sprintf('%s', $v);"), []);
    assert_eq!(evidence("$v", "sprintf('%s', $_GET);"), []);
}

/// A property, a constant and a call carry their name, for the engine to read the declared
/// type or value of; any other property or callee is no evidence.
#[test]
fn a_printf_property_constant_or_call_carries_its_name() {
    use steins_syntax::{NameRef, RefKind, StaticClass};
    let shown = evidence(
        "$v",
        "sprintf('%s%s%s%s%s%s%s', $this->p, strlen($v), $o->q, self::$p, PHP_EOL, self::K, Foo::K);",
    );
    assert_eq!(shown[0], (1, shape_of(ArgShape::ThisProperty("p".to_owned()))));
    let strlen = NameRef { raw: "strlen".to_owned(), kind: RefKind::Unqualified, offset: 0 };
    assert_eq!(shown[1], (2, shape_of(ArgShape::Call(strlen))));
    assert_eq!(
        shown[2],
        (4, FloatEvidence::StaticProperty { class: StaticClass::SelfKw, name: "p".to_owned() })
    );
    assert!(matches!(&shown[3], (5, FloatEvidence::GlobalConst(n)) if n.raw == "PHP_EOL"));
    assert_eq!(
        shown[4],
        (6, FloatEvidence::ClassConst { class: StaticClass::SelfKw, name: "K".to_owned() })
    );
    assert!(matches!(&shown[5], (7, FloatEvidence::ClassConst { class: StaticClass::Named(_), .. })));
    assert_eq!(shown.len(), 6, "`$o->q` shows nothing");
    assert_eq!(evidence("$v", "sprintf('%s', $f());"), []);
    assert!(matches!(
        evidence("$v", "sprintf('%s', \\App\\K);").as_slice(),
        [(1, FloatEvidence::GlobalConst(n))] if n.raw.ends_with("App\\K")
    ));
    assert_eq!(evidence("$v", "sprintf('%s', static::K);"), [(
        1,
        FloatEvidence::ClassConst { class: StaticClass::Static, name: "K".to_owned() }
    )]);
    // A method call names its receiver where the scan can: `$this`, `self`, a class.
    let got = evidence("$v", "sprintf('%s%s', $this->m(), Foo::m());");
    assert_eq!(got.len(), 2);
}
