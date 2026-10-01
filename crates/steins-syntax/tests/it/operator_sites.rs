//! The operator sites (ADR-0099 §4.3, issue #859): a syntactic form through
//! which the engine may run a user method on an operand lowers to a
//! [`SiteKind::Operator`], with the operands' shapes and what names each
//! operand's class. One snippet per family and construct, and the cases where
//! no site is emitted.

use steins_syntax::{
    ArgShape, EffectRecv, OperatorConstruct as C, OperatorFamily as F, SiteKind, SourceTree, Stored,
    derive_effect_origins, derive_throw_origins,
};

/// One operator site as the lowering recorded it.
#[derive(Debug, Clone, PartialEq)]
struct Op {
    family: F,
    construct: C,
    member: Option<String>,
    operands: Vec<ArgShape>,
    receivers: Vec<Option<EffectRecv>>,
}

/// The operator sites of `K::f($o, array $a, string $s)` for `body`, in site
/// order. `K` has a `$repo` property so `$this->repo` is a declared receiver.
fn ops(body: &str) -> Vec<Op> {
    let src = format!(
        "<?php\nclass K {{ private $repo; public function f($o, array $a, string $s) {{ {body} }} }}\n"
    );
    let tree = SourceTree::parse(&src);
    assert!(tree.parse_errors().is_empty(), "{body}: {:?}", tree.parse_errors());
    let method = &tree.classes()[0].methods[0];
    // No lane has a view of an operator site.
    assert!(
        !derive_effect_origins(&method.sites).iter().any(|o| format!("{o:?}").contains("Operator"))
    );
    method
        .sites
        .iter()
        .filter_map(|site| match &site.kind {
            SiteKind::Operator { family, construct, receivers, member } => Some(Op {
                family: *family,
                construct: *construct,
                member: member.clone(),
                operands: site.operands.clone().expect("an operator site carries operands"),
                receivers: receivers.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// The `(family, construct)` pairs of `body`'s operator sites.
fn forms(body: &str) -> Vec<(F, C)> {
    ops(body).iter().map(|o| (o.family, o.construct)).collect()
}

/// `body` lowers to exactly one operator site; return it.
fn one(body: &str) -> Op {
    let mut all = ops(body);
    assert_eq!(all.len(), 1, "{body}: {all:?}");
    all.remove(0)
}

/// The shape of the untyped or `string` parameter `name`, given no write.
fn param(name: &str) -> ArgShape {
    ArgShape::Param { name: name.to_owned(), stores: Stored::ObjectFree }
}

fn to_string(construct: C) -> (F, C) {
    (F::ToString, construct)
}

#[test]
fn concatenation_records_both_sides() {
    let op = one("return $o . 'x';");
    assert_eq!((op.family, op.construct), to_string(C::Concat));
    assert_eq!(op.operands, [param("o"), ArgShape::ObjectFree]);
    assert_eq!(op.receivers, [Some(EffectRecv::Var("o".to_owned())), None]);
    // A chain is one site per `.`; the inner one is object-free to the outer.
    let chain = ops("return $o . 'x' . $this;");
    assert_eq!(chain.len(), 2, "{chain:?}");
    assert_eq!(chain[0].operands, [ArgShape::ObjectFree, ArgShape::Unknown]);
    assert_eq!(chain[0].receivers, [None, Some(EffectRecv::This)]);
    assert_eq!(chain[1].operands, [param("o"), ArgShape::ObjectFree]);
    assert_eq!(forms("$s .= $o;"), [to_string(C::ConcatAssign)]);
}

#[test]
fn a_site_names_each_operands_class_like_a_method_receiver() {
    let op = one("return new Foo . $o;");
    assert_eq!(op.receivers.len(), 2);
    assert!(matches!(&op.receivers[0], Some(EffectRecv::ClassName(n)) if n.simple() == "Foo"));
    // `$o` is never written here, so it is a declared receiver; written, it is not.
    assert_eq!(op.receivers[1], Some(EffectRecv::Var("o".to_owned())));
    assert_eq!(one("$o = h(); return $o . 'x';").receivers, [None, None]);
    // `$this->repo` is itself a property read, and the concatenation names it as a declared property.
    let this_prop = ops("return $this->repo . 'x';");
    assert_eq!(this_prop[0].receivers[0], Some(EffectRecv::PropRead("repo".to_owned())));
    assert_eq!(this_prop[0].operands[0], ArgShape::ThisProperty("repo".to_owned()));
    assert_eq!((this_prop[1].family, this_prop[1].construct), (F::MagicProp, C::Read));
}

#[test]
fn interpolation_and_heredoc_record_the_embedded_expressions() {
    let op = one(r#"return "a{$o}b$s";"#);
    assert_eq!((op.family, op.construct), to_string(C::Interpolation));
    assert_eq!(op.operands.len(), 2);
    assert_eq!(op.operands[0], param("o"));
    let doc = one("return <<<T\n  v {$o}\n  T;\n");
    assert_eq!((doc.family, doc.construct), to_string(C::Heredoc));
    // A nowdoc embeds nothing.
    assert_eq!(forms("return <<<'T'\n  v $o\n  T;\n"), []);
}

#[test]
fn a_cast_echo_and_print_convert_their_operands() {
    assert_eq!(forms("return (string) $o;"), [to_string(C::Cast)]);
    assert_eq!(forms("return (binary) $o;"), [to_string(C::Cast)]);
    assert_eq!(forms("return (int) $o;"), []);
    let echo = &ops("echo $o, 'x', $this->repo;")[0];
    assert_eq!(echo.construct, C::Echo);
    assert_eq!(echo.operands, [param("o"), ArgShape::ObjectFree, ArgShape::ThisProperty("repo".to_owned())]);
    assert_eq!(forms("print $o;"), [to_string(C::Print)]);
    assert_eq!(forms("?><?= $o ?><?php "), [to_string(C::Echo)]);
}

#[test]
fn comparisons_convert_an_object_only_against_a_string() {
    assert_eq!(forms("return $o == $s;"), [to_string(C::LooseCompare)]);
    assert_eq!(forms("return $o != 'a';"), [to_string(C::LooseCompare)]);
    assert_eq!(forms("return $o <> $s;"), [to_string(C::LooseCompare)]);
    for op in ["<", "<=", ">", ">=", "<=>"] {
        assert_eq!(forms(&format!("return $o {op} $s;")), [to_string(C::OrderCompare)], "{op}");
    }
    // Strict comparison converts nothing; a non-string scalar converts no object to a string.
    assert_eq!(forms("return $o === $s;"), []);
    assert_eq!(forms("return $o == null || $o == 0 || $o != -1 || $o < 1.5 || $o == true;"), []);
}

#[test]
fn a_switch_compares_its_subject_with_every_case() {
    let op = one("switch ($o) { case 'a': return 1; case $s: return 2; default: return 3; }");
    assert_eq!((op.family, op.construct), to_string(C::Switch));
    assert_eq!(op.operands.len(), 3, "subject and two cases: {op:?}");
    assert_eq!(forms("switch ($o) { case 1: return 1; case null: return 2; }"), []);
    assert_eq!(forms("switch (true) { case 'a': return 1; }"), []);
}

#[test]
fn a_property_access_takes_the_role_its_context_gives_it() {
    let read = one("return $o->p;");
    assert_eq!((read.family, read.construct), (F::MagicProp, C::Read));
    assert_eq!(read.member.as_deref(), Some("p"));
    assert_eq!(read.operands, [param("o")]);
    assert_eq!(read.receivers, [Some(EffectRecv::Var("o".to_owned()))]);
    let magic = |body: &str| forms(body);
    let prop = |c: C| (F::MagicProp, c);
    assert_eq!(magic("$o->p = 1;"), [prop(C::Write)]);
    assert_eq!(magic("$o->p += 1;"), [prop(C::ReadWrite)]);
    assert_eq!(magic("$o->p++;"), [prop(C::ReadWrite)]);
    assert_eq!(magic("--$o->p;"), [prop(C::ReadWrite)]);
    assert_eq!(magic("return isset($o->p);"), [prop(C::Isset)]);
    assert_eq!(magic("return empty($o->p);"), [prop(C::Empty)]);
    assert_eq!(magic("unset($o->p);"), [prop(C::Unset)]);
    assert_eq!(magic("return $o->p ?? 1;"), [prop(C::Coalesce)]);
    assert_eq!(magic("$o->p ??= 1;"), [prop(C::CoalesceAssign)]);
    assert_eq!(magic("$r = &$o->p;"), [prop(C::Reference)]);
    assert_eq!(magic("return $o?->p;"), [prop(C::Read)]);
    assert_eq!(one("$o->$s = 1;").member, None, "a dynamic name has no member");
}

#[test]
fn a_chain_gives_the_outermost_access_the_role_and_the_rest_a_fetch() {
    let prop = |c: C| (F::MagicProp, c);
    assert_eq!(forms("$o->a->b = 1;"), [prop(C::Write), prop(C::Read)]);
    assert_eq!(forms("return isset($o->a->b);"), [prop(C::Isset), prop(C::Isset)]);
    assert_eq!(forms("unset($o->a->b);"), [prop(C::Unset), prop(C::Read)]);
    // The names follow the nesting: the outer access is `b`, its object `$o->a`.
    let chain = ops("return $o->a->b;");
    assert_eq!(chain[0].member.as_deref(), Some("b"));
    assert_eq!(chain[1].member.as_deref(), Some("a"));
    assert_eq!(chain[0].operands, [ArgShape::Unknown]);
    // An offset write through a property: the property is fetched, the offset written.
    let mixed = forms("$o->p['k'] = 1;");
    assert_eq!(mixed, [(F::ArrayAccess, C::Write), (F::MagicProp, C::Read)]);
}

#[test]
fn a_this_property_is_a_site_with_the_this_receiver() {
    let op = one("return $this->repo;");
    assert_eq!((op.family, op.construct), (F::MagicProp, C::Read));
    assert_eq!(op.member.as_deref(), Some("repo"));
    assert_eq!(op.receivers, [Some(EffectRecv::This)]);
    let write = one("$this->repo = $o;");
    assert_eq!(write.construct, C::Write);
    assert_eq!(write.receivers, [Some(EffectRecv::This)]);
}

#[test]
fn an_offset_access_takes_the_role_its_context_gives_it() {
    let read = one("return $o['k'];");
    assert_eq!((read.family, read.construct), (F::ArrayAccess, C::Read));
    assert_eq!(read.member, None);
    assert_eq!(read.operands, [param("o")]);
    let offset = |c: C| (F::ArrayAccess, c);
    assert_eq!(forms("$o['k'] = 1;"), [offset(C::Write)]);
    assert_eq!(forms("$o[] = 1;"), [offset(C::Write)]);
    assert_eq!(forms("$o['k'] .= 'x';"), [to_string(C::ConcatAssign), offset(C::ReadWrite)]);
    assert_eq!(forms("return isset($o['k']);"), [offset(C::Isset)]);
    assert_eq!(forms("return empty($o['k']);"), [offset(C::Empty)]);
    assert_eq!(forms("unset($o['k']);"), [offset(C::Unset)]);
    assert_eq!(forms("return $o['k'] ?? 1;"), [offset(C::Coalesce)]);
    assert_eq!(forms("$o['k'] ??= 1;"), [offset(C::CoalesceAssign)]);
    assert_eq!(forms("$r = &$o['k'];"), [offset(C::Reference)]);
}

#[test]
fn destructuring_is_an_offset_access_on_the_value_and_writes_its_targets() {
    let op = &ops("[$x, $y] = $o;")[0];
    assert_eq!((op.family, op.construct), (F::ArrayAccess, C::Destructure));
    assert_eq!(op.operands, [param("o")]);
    assert_eq!(op.receivers, [Some(EffectRecv::Var("o".to_owned()))]);
    assert_eq!(forms("list('k' => $x) = $o;"), [(F::ArrayAccess, C::Destructure)]);
    // A target is written: `$o->p` goes through `__set`, and a nested pattern
    // destructures an element nothing names.
    let nested = ops("[$this->repo, [$x]] = $o;");
    let shapes: Vec<_> = nested.iter().map(|o| (o.family, o.construct)).collect();
    assert_eq!(
        shapes,
        [
            (F::ArrayAccess, C::Destructure),
            (F::MagicProp, C::Write),
            (F::ArrayAccess, C::Destructure)
        ]
    );
    assert_eq!(nested[2].operands, [ArgShape::Unknown]);
    // An array literal on the right holds no object.
    assert_eq!(forms("[$x, $y] = [1, 2];"), []);
    // A `foreach` target is written; a pattern there destructures each row.
    let foreach = (F::Iterate, C::Foreach);
    assert_eq!(forms("foreach ($a as $this->repo) {}"), [foreach, (F::MagicProp, C::Write)]);
    assert_eq!(forms("foreach ($a as [$x, $y]) {}"), [foreach, (F::ArrayAccess, C::Destructure)]);
}

#[test]
fn iteration_records_its_operand() {
    let it = |c: C| (F::Iterate, c);
    let each = one("foreach ($o as $v) {}");
    assert_eq!((each.family, each.construct), it(C::Foreach));
    assert_eq!(each.operands, [param("o")]);
    assert_eq!(forms("foreach ($o as $k => $v) {}"), [it(C::Foreach)]);
    assert_eq!(forms("yield from $o;"), [it(C::YieldFrom)]);
    assert_eq!(forms("return f(...$o);"), [it(C::Spread)]);
    assert_eq!(forms("return [...$o];"), [it(C::Spread)]);
    // An array holds no object to iterate.
    assert_eq!(forms("foreach ([1, 2] as $v) {}"), []);
    assert_eq!(forms("foreach ([$o] as $v) {}"), []);
}

#[test]
fn clone_records_the_cloned_expression() {
    let op = one("return clone $this;");
    assert_eq!((op.family, op.construct), (F::Clone, C::Clone));
    assert_eq!(op.receivers, [Some(EffectRecv::This)]);
    assert_eq!(forms("return clone $o;"), [(F::Clone, C::Clone)]);
    assert_eq!(forms("return clone new Foo;"), [(F::Clone, C::Clone)]);
}

#[test]
fn an_operand_shown_to_hold_no_object_emits_no_site() {
    assert_eq!(forms("return 'a' . 'b' . 1 . \"c\";"), []);
    assert_eq!(forms("return \"x{$s}\";").len(), 1, "a parameter is not shown to hold none");
}

#[test]
fn an_array_expression_emits_no_site_but_a_variable_does() {
    assert_eq!(forms("echo 'a', 1;"), []);
    assert_eq!(forms("return [1, 2]['k'];"), []);
    // A variable keeps its site however the frame writes it: a named call may
    // take it by reference and store an object into it, which only the resolver
    // can tell. A local is no exception.
    let local = one("$l = 'x'; return $l . 'y';");
    assert_eq!(local.operands[0], ArgShape::Local { name: "l".to_owned(), stores: Stored::ObjectFree });
    assert_eq!(forms("$l = [1]; return $l['k'];"), [(F::ArrayAccess, C::Read)]);
    assert_eq!(forms("setv($v); echo $v;"), [(F::ToString, C::Echo)]);
    let param = one("return $s . 'y';");
    assert_eq!(param.operands[0], ArgShape::Param { name: "s".to_owned(), stores: Stored::ObjectFree });
    assert_eq!(forms("return $a['k'];"), [(F::ArrayAccess, C::Read)]);
    // A local given an unknown value is unknown.
    assert_eq!(forms("$l = h(); return $l . 'y';"), [to_string(C::Concat)]);
}

#[test]
fn operator_sites_are_not_part_of_either_lane() {
    let src = "<?php\nfunction f($o) { return $o . 'x' . $o->p; }\n";
    let tree = SourceTree::parse(src);
    let f = &tree.functions()[0];
    assert_eq!(f.sites.iter().filter(|s| matches!(s.kind, SiteKind::Operator { .. })).count(), 3);
    assert!(derive_effect_origins(&f.sites).is_empty());
    assert!(derive_throw_origins(&f.sites).is_empty());
}

#[test]
fn a_site_under_a_catch_guard_carries_it_and_nested_scopes_own_theirs() {
    let src = "<?php\nfunction f($o) { try { echo $o; } catch (\\Exception $e) {} $c = function () use ($o) { return $o . 'x'; }; }\n";
    let tree = SourceTree::parse(src);
    let f = &tree.functions()[0];
    let own: Vec<_> = f.sites.iter().filter(|s| matches!(s.kind, SiteKind::Operator { .. })).collect();
    assert_eq!(own.len(), 1, "the closure's `.` is the closure's: {own:?}");
    assert!(!own[0].guards.is_empty(), "the echo sits in a try");
    let closure = tree.scopes().iter().find(|s| !s.sites.is_empty()).expect("the closure scope");
    assert!(closure.sites.iter().any(|s| matches!(s.kind, SiteKind::Operator { .. })));
}

#[test]
fn clone_with_a_property_list_is_a_clone_site_beside_the_call_the_parser_reads() {
    let with = one("return clone($o, ['a' => 1]);");
    assert_eq!((with.family, with.construct), (F::Clone, C::CloneWith));
    assert_eq!(with.operands, [param("o")]);
    // The frame passes `$o` to what it reads as a function, which may take it by
    // reference, so `$o` is not a never-written variable and names no class.
    assert_eq!(with.receivers, [None]);
    assert_eq!(forms("return clone(object: $o, withProperties: []);"), [(F::Clone, C::CloneWith)]);
    // A function that merely ends in `clone` is a call and nothing else.
    assert_eq!(forms("return my_clone($o, []);"), []);
}

#[test]
fn a_comparison_of_an_array_that_may_hold_an_object_keeps_its_site() {
    // PHP 8.5: `[$o] == ['x']`, `[$o] < ['y']`, `[$o] <=> ['y']` and a `switch` over
    // `[$o]` with `case ['x']` run the element's `__toString`.
    let held = "$l = [$o]; ";
    assert_eq!(forms(&format!("{held}return $l == ['x'];")), [to_string(C::LooseCompare)]);
    assert_eq!(forms(&format!("{held}return $l != ['x'];")), [to_string(C::LooseCompare)]);
    assert_eq!(forms(&format!("{held}return $l < ['y'];")), [to_string(C::OrderCompare)]);
    assert_eq!(forms(&format!("{held}return $l <=> $l;")), [to_string(C::OrderCompare)]);
    assert_eq!(forms(&format!("{held}return [$o] == ['x'];")), [to_string(C::LooseCompare)]);
    assert_eq!(
        forms(&format!("{held}switch ($l) {{ case ['x']: return 1; }}")),
        [to_string(C::Switch)]
    );
    let op = &ops(&format!("{held}return $l == ['x'];"))[0];
    assert_eq!(op.operands[0], ArgShape::Local { name: "l".to_owned(), stores: Stored::Array });
}

#[test]
fn a_comparison_of_values_holding_no_object_or_against_a_scalar_literal_emits_no_site() {
    // An object-free expression compares without touching an object.
    assert_eq!(forms("return 'x' == 'y';"), []);
    assert_eq!(forms("return [1] == ['x'] || [1] < [2];"), []);
    assert_eq!(forms("switch ('x') { case 'y': return 1; }"), []);
    // PHP 8.5: an array holding an object against `null`, a boolean, an integer or a
    // float compares without touching its elements, so the literal rule holds.
    let held = "$l = [$o]; ";
    for rhs in ["null", "true", "false", "1", "-1", "1.5"] {
        assert_eq!(forms(&format!("{held}return $l == {rhs} || $l < {rhs};")), [], "{rhs}");
    }
    assert_eq!(
        forms(&format!("{held}switch ($l) {{ case 1: return 1; case null: return 2; }}")),
        []
    );
    // The wider rule still holds where nothing compares: conversion and access
    // leave an array's elements alone.
    assert_eq!(forms("return [$o] . 'x';"), []);
    assert_eq!(forms("return (string) [$o];"), []);
    assert_eq!(forms("echo [$o]; foreach ([$o] as $v) {} return [$o]['k'];"), []);
}
