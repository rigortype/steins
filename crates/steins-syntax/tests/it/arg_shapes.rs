//! What the effect scan shows a call argument holds (issue #856): the
//! `ArgShape` a call origin carries per positional argument, by the
//! argument's form or by a flow-insensitive summary of the frame's writes.

use steins_syntax::{ArgShape, EffectOrigin, SourceTree, Stored};

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
    f.effect_origins
        .iter()
        .find_map(g_shapes)
        .unwrap_or_else(|| panic!("no positional call to g in {:?}", f.effect_origins))
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
    assert_eq!(shapes("$v", "g([$v], (array) $v, $v + 1, PHP_EOL, h(), $v ? 'a' : 'b');"), [
        Array, Array, Unknown, Unknown, Unknown, ObjectFree
    ]);
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
        .flat_map(|s| s.effect_origins.iter())
        .filter_map(g_shapes)
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
    for origin in &m.effect_origins {
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
