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
    // A dynamic name has no member, and its conversion is a site of its own.
    let dynamic = ops("$o->$s = 1;");
    assert_eq!(dynamic[0].member, None);
    assert_eq!(forms("$o->$s = 1;"), [prop(C::Write), to_string(C::Name)]);
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

// ---- Name operands and offset-write values (ADR-0099 §4.3, #880) -------------

/// The Name sites of `body`, in the harness's `K::f($o, array $a, string $s)`.
fn names(body: &str) -> Vec<Op> {
    ops(body).into_iter().filter(|o| o.construct == C::Name).collect()
}

#[test]
fn a_dynamic_property_name_is_a_to_string_site_on_the_name_expression() {
    // The access itself stays a MagicProp read, in source order before the name.
    assert_eq!(forms("return $o->$s;"), [(F::MagicProp, C::Read), to_string(C::Name)]);
    let name = &names("return $o->$s;")[0];
    assert_eq!(name.operands, [param("s")]);
    assert_eq!(name.receivers, [Some(EffectRecv::Var("s".to_owned()))]);
    // Written, `{$e}`, nullsafe, and as an lvalue.
    assert_eq!(names("return $o->{$o};")[0].operands, [param("o")]);
    assert_eq!(names("return $o?->$s;").len(), 1);
    assert_eq!(names("$o->$s = 1;").len(), 1);
    assert_eq!(names("unset($o->$s);").len(), 1);
    assert_eq!(names("return isset($o->$s->$s);").len(), 2);
    // A name that is no object shows none; an identifier has no name expression; a
    // dynamic method name is a dynamic callee, not a conversion of this kind.
    assert_eq!(names("return $o->{'x'};"), []);
    assert_eq!(names("return $o->{$s . 'x'};"), []);
    assert_eq!(names("return $o->x;"), []);
    assert_eq!(names("return $o->$s();"), []);
}

#[test]
fn a_variable_variable_converts_the_name_it_is_read_by() {
    let nested = names("return $$s;");
    assert_eq!(nested.len(), 1, "{nested:?}");
    // The frame holds a `$$`, so no name of it is shown (ADR-0001's give-up list).
    assert_eq!(nested[0].operands, [ArgShape::Unknown]);
    // `${expr}` converts the expression; `$$$n` converts `$$n` and then `$n`.
    assert_eq!(names("return ${$o};").len(), 1);
    assert_eq!(names("return ${$s . 'x'};"), []);
    assert_eq!(names("return ${'x'};"), []);
    let triple = names("return $$$s;");
    assert_eq!(triple.len(), 2, "{triple:?}");
    // As an lvalue and in a by-reference position the name converts as well.
    assert_eq!(names("$$s = 1;").len(), 1);
    assert_eq!(names("return isset($$s);").len(), 1);
}

#[test]
fn a_static_property_name_is_converted_and_a_plain_one_is_not() {
    let site = names("return K::$$s;");
    assert_eq!(site.len(), 1, "{site:?}");
    assert_eq!(names("return K::$repo;"), []);
    assert_eq!(names("return K::${'x'};"), []);
}

/// The one offset-write value site of `body`.
fn offset_value(body: &str) -> Op {
    let all: Vec<Op> = ops(body).into_iter().filter(|o| o.construct == C::OffsetValue).collect();
    assert_eq!(all.len(), 1, "{body}: {all:?}");
    all.into_iter().next().unwrap()
}

#[test]
fn an_offset_write_records_its_value_and_what_its_container_is_shown_to_be() {
    let write = offset_value("$s[0] = $o;");
    assert_eq!((write.family, write.construct), to_string(C::OffsetValue));
    // `$s` is a `string` parameter: no non-string shape, though the write is an element write.
    assert_eq!(write.operands, [param("o"), ArgShape::Unknown]);
    assert_eq!(write.receivers, [Some(EffectRecv::Var("o".to_owned())), None]);
    // An `array` parameter no whole-variable write touches is shown not to be a string.
    let array = ArgShape::Param { name: "a".to_owned(), stores: Stored::Array };
    assert_eq!(offset_value("$a[0] = $o;").operands, [param("o"), array.clone()]);
    assert_eq!(offset_value("$a['k'] = $o;").operands[1], array);
    // `$this->repo` is read by the engine against the property's declared type.
    assert_eq!(
        offset_value("$this->repo[0] = $o;").operands[1],
        ArgShape::ThisProperty("repo".to_owned()),
    );
    // Not a variable or `$this->p`: an element, a call, another object's property.
    assert_eq!(offset_value("$a[1][0] = $o;").operands[1], ArgShape::Unknown);
    assert_eq!(offset_value("$o->p[0] = $o;").operands[1], ArgShape::Unknown);
    assert_eq!(offset_value("f()[0] = $o;").operands[1], ArgShape::Unknown);
}

#[test]
fn a_local_container_is_a_non_string_only_while_every_whole_write_shows_it() {
    let local = |stores| ArgShape::Local { name: "l".to_owned(), stores };
    let container = |body: &str| offset_value(body).operands[1].clone();
    // Never written whole (an offset write makes `null` an array), array literals,
    // `null`, numbers, booleans and objects.
    assert_eq!(container("$l[0] = $o;"), local(Stored::Array));
    assert_eq!(container("$l = []; $l[0] = $o;"), local(Stored::Array));
    assert_eq!(container("$l = [1, 2]; $l = array(); $l[0] = $o;"), local(Stored::Array));
    assert_eq!(container("$l = null; $l[0] = $o;"), local(Stored::Array));
    assert_eq!(container("$l = new K; $l[0] = $o;"), local(Stored::Array));
    assert_eq!(container("$l = (array) $s; $l[0] = $o;"), local(Stored::Array));
    assert_eq!(container("$l = []; $l += [1]; $l[0] = $o;"), local(Stored::Array));
    // A string, a call, another variable, `.=`, a destructuring target, a `foreach`
    // binding: any of them may leave a string.
    for write in [
        "$l = 'abc';",
        "$l = f();",
        "$l = $s;",
        "$l = []; $l .= 'x';",
        "$l = []; $l = $a;",
        "[$l] = $a;",
        "list('k' => $l) = $a;",
        "foreach ($a as $l) {}",
        "try { f(); } catch (Exception $l) {}",
    ] {
        assert_eq!(container(&format!("{write} $l[0] = $o;")), ArgShape::Unknown, "{write}");
    }
    // The write after the offset write counts as well: the scan is flow-insensitive.
    assert_eq!(container("$l[0] = $o; $l = 'abc';"), ArgShape::Unknown);
}

#[test]
fn a_parameter_container_is_a_non_string_only_by_its_declared_type() {
    let container = |hint: &str, body: &str| {
        let src = format!("<?php\nclass K {{ public function f({hint} $c, $o) {{ {body} }} }}\n");
        let tree = SourceTree::parse(&src);
        assert!(tree.parse_errors().is_empty(), "{hint}: {:?}", tree.parse_errors());
        let site = tree.classes()[0].methods[0]
            .sites
            .iter()
            .find(|s| matches!(&s.kind, SiteKind::Operator { construct: C::OffsetValue, .. }))
            .expect("an offset-write value site");
        site.operands.clone().unwrap()[1].clone()
    };
    let array = ArgShape::Param { name: "c".to_owned(), stores: Stored::Array };
    for hint in ["array", "?array", "array|null", "K", "?K", "\\ArrayAccess", "int", "iterable", "object"]
    {
        assert_eq!(container(hint, "$c[0] = $o;"), array, "{hint}");
    }
    // A type that admits a string, or none at all, may be one.
    for hint in ["string", "?string", "array|string", "mixed", "callable", ""] {
        assert_eq!(container(hint, "$c[0] = $o;"), ArgShape::Unknown, "{hint}");
    }
    // A whole-variable write the declared type does not vouch for rules it out.
    assert_eq!(container("array", "$c = 'abc'; $c[0] = $o;"), ArgShape::Unknown);
    assert_eq!(container("array", "$c = []; $c[0] = $o;"), array);
}

#[test]
fn an_offset_write_of_a_value_holding_no_object_or_an_append_emits_no_value_site() {
    let writes = |body: &str| forms(body).into_iter().filter(|f| f.1 == C::OffsetValue).count();
    assert_eq!(writes("$s[0] = 'z';"), 0);
    assert_eq!(writes("$s[0] = 1 . 'x';"), 0);
    assert_eq!(writes("$s[0] = [$o];"), 0);
    // `$c[] = v` on a string is a fatal error, never a conversion.
    assert_eq!(writes("$s[] = $o;"), 0);
    // A compound assignment into an offset is not a plain write.
    assert_eq!(writes("$a[0] .= $o;"), 0);
    assert_eq!(writes("$s[0] = $o;"), 1);
    // `??=` stores the value as `=` does when the offset is unset (witnessed), so it converts too.
    assert_eq!(writes("$s[5] ??= $o;"), 1);
    assert_eq!(writes("$s[5] ??= 'z';"), 0);
}

#[test]
fn a_destructuring_or_foreach_target_that_is_an_offset_stores_an_unknown_value() {
    let list = offset_value("[$s[0]] = $a;");
    assert_eq!(list.operands, [ArgShape::Unknown, ArgShape::Unknown]);
    assert_eq!(list.receivers, [None, None]);
    assert_eq!(offset_value("foreach ($a as $s[0]) {}").operands[0], ArgShape::Unknown);
}

/// The MagicProp `Write` sites of a constructor, by the member they name.
fn promoted(params: &str, body: &str) -> Vec<(Option<String>, Vec<Option<EffectRecv>>)> {
    let src = format!("<?php\nclass K {{ public function __construct({params}) {{ {body} }} }}\n");
    let tree = SourceTree::parse(&src);
    assert!(tree.parse_errors().is_empty(), "{params}: {:?}", tree.parse_errors());
    tree.classes()[0].methods[0]
        .sites
        .iter()
        .filter_map(|s| match &s.kind {
            SiteKind::Operator { family: F::MagicProp, construct: C::Write, member, receivers } => {
                Some((member.clone(), receivers.clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_hooked_promoted_parameter_is_a_magic_property_write_on_this_in_the_constructor() {
    let hooked = promoted("public string $p { set(string $v) { $this->p = $v; } }", "");
    assert_eq!(hooked, [(Some("p".to_owned()), vec![Some(EffectRecv::This)])]);
    // One per parameter with a `set` hook; a `get`-only hook runs nothing at promotion,
    // and neither does a plain promoted parameter or an ordinary one.
    let two = promoted(
        "public string $a { get => 'x'; }, public int $b, string $c, public int $d { set => 1; }, \
         public int $e { get => 1; set => 2; }",
        "",
    );
    assert_eq!(two.iter().map(|s| s.0.as_deref()).collect::<Vec<_>>(), [Some("d"), Some("e")]);
    assert_eq!(promoted("public string $p", ""), []);
    assert_eq!(promoted("string $p", ""), []);
    // The prologue runs before the body: the site leads the constructor's list.
    let src = "<?php\nclass K { public function __construct(public int $p { set => 1; }) { echo 'x'; } }\n";
    let tree = SourceTree::parse(src);
    let sites = &tree.classes()[0].methods[0].sites;
    assert!(matches!(sites[0].kind, SiteKind::Operator { member: Some(_), .. }), "{sites:?}");
    // A method other than a constructor cannot promote.
    let plain = SourceTree::parse("<?php\nclass K { public function m(int $p) {} }\n");
    assert!(plain.classes()[0].methods[0].sites.is_empty());
}

// ---- Drop sites (ADR-0100 §7, issue #882) -------------------------------------

/// One drop site of `function f(params) { body }`: its construct, the receivers it
/// carries (`C` for the exact class `C`, `~C` for `C` or a subclass, `self` and `parent`
/// for those hints, `?` for a class the lowering knows runs user code), and the source
/// text its span covers.
type Drop = (C, Vec<String>, String);

fn drops(params: &str, body: &str) -> Vec<Drop> {
    let src = format!("<?php\nfunction f({params}) {{ {body} }}\n");
    let tree = SourceTree::parse(&src);
    assert!(tree.parse_errors().is_empty(), "{body}: {:?}", tree.parse_errors());
    tree.functions()[0]
        .sites
        .iter()
        .filter_map(|site| match &site.kind {
            SiteKind::Operator { family: F::Drop, construct, receivers, member } => {
                assert_eq!(*member, None);
                // One operand per receiver, none of which names a shape of its own.
                let operands = site.operands.as_ref().expect("a drop site carries operands");
                assert_eq!(operands, &vec![ArgShape::Unknown; receivers.len()]);
                let receivers = receivers
                    .iter()
                    .map(|r| match r {
                        Some(EffectRecv::ClassName(name)) => name.raw.clone(),
                        Some(EffectRecv::Bound(name)) => format!("~{}", name.raw),
                        Some(EffectRecv::SelfKw) => "self".to_owned(),
                        Some(EffectRecv::Parent) => "parent".to_owned(),
                        None => "?".to_owned(),
                        other => panic!("{other:?}"),
                    })
                    .collect();
                let text = src[site.span.start as usize..site.span.end as usize].to_owned();
                Some((*construct, receivers, text))
            }
            _ => None,
        })
        .collect()
}

fn drop_site(construct: C, receivers: &[&str], text: &str) -> Drop {
    (construct, receivers.iter().map(|r| (*r).to_owned()).collect(), text.to_owned())
}

#[test]
fn a_local_first_written_with_new_drops_at_unset_reassignment_and_scope_exit() {
    let sites = drops("", "$f = new D; unset($f); $f = null; echo 'x';");
    assert_eq!(
        sites,
        [
            drop_site(C::DropUnset, &["D"], "$f"),
            drop_site(C::DropReassign, &["D"], "$f = null"),
            drop_site(C::DropScopeExit, &["D"], "}"),
        ]
    );
    // Every form is of the one family.
    for construct in [C::DropUnset, C::DropReassign, C::DropScopeExit, C::DropTemporary] {
        assert_eq!(construct.family_hint(), Some(F::Drop));
    }
}

#[test]
fn the_first_write_releases_nothing_unless_a_loop_or_goto_runs_it_again() {
    assert_eq!(drops("", "$f = new D;"), [drop_site(C::DropScopeExit, &["D"], "}")]);
    for body in [
        "while ($c) { $f = new D; }",
        "foreach ($xs as $x) { $f = new D; }",
        "for (;;) { $f = new D; }",
        "do { $f = new D; } while ($c);",
        "a: $f = new D; goto a;",
    ] {
        let sites = drops("", body);
        assert!(
            sites.iter().any(|s| s.0 == C::DropReassign && s.2 == "$f = new D"),
            "{body}: {sites:?}"
        );
    }
}

#[test]
fn a_local_remembers_every_class_it_is_written_with_a_new() {
    let sites = drops("", "$f = new D; $f = new E; $f = new D; $f = new E;");
    let reassign = |text: &str| drop_site(C::DropReassign, &["D", "E"], text);
    assert_eq!(sites[0], reassign("$f = new E"));
    assert_eq!(sites[3], drop_site(C::DropScopeExit, &["D", "E"], "}"));
    assert_eq!(sites.len(), 4, "{sites:?}");
}

#[test]
fn a_local_first_written_with_anything_but_a_new_is_no_subject() {
    for body in [
        "$f = make(); unset($f); $f = new D;",
        "$f = null; $f = new D; unset($f);",
        "$f = new $c; unset($f);",
        "$f = new static; unset($f);",
        "$f = new self; unset($f);",
    ] {
        let sites = drops("", body);
        assert!(!sites.iter().any(|s| s.2 == "$f" || s.0 == C::DropScopeExit), "{body}: {sites:?}");
    }
    // The inner write of a chain is a write of its own variable, not of the outer one.
    assert_eq!(
        drops("", "$a = $f = new D; unset($a);"),
        [drop_site(C::DropScopeExit, &["D"], "}")]
    );
    // `??=` assigns only an unset variable, so it is no write of a value to drop.
    assert_eq!(drops("", "$f ??= new D; unset($f);"), []);
}

#[test]
fn a_parameter_is_a_subject_by_its_hint_even_where_the_frame_writes_it() {
    let sites = drops("D $d", "$d = null; unset($d);");
    assert_eq!(
        sites,
        [
            drop_site(C::DropReassign, &["~D"], "$d = null"),
            drop_site(C::DropUnset, &["~D"], "$d"),
            drop_site(C::DropScopeExit, &["~D"], "}"),
        ]
    );
    // Every hint that can name a class: nullable, union, intersection.
    for hint in ["?D", "D|int", "D&E", "int|D|null"] {
        assert_eq!(drops(&format!("{hint} $d"), "").len(), 1, "{hint}");
    }
}

#[test]
fn a_hint_names_every_class_member_whatever_else_it_holds() {
    let exit = |receivers: &[&str]| [drop_site(C::DropScopeExit, receivers, "}")];
    assert_eq!(drops("array|D $x", ""), exit(&["~D"]));
    assert_eq!(drops("D|E|array|int|null $x", ""), exit(&["~D", "~E"]));
    assert_eq!(drops("mixed|D $x", ""), exit(&["~D"]));
    assert_eq!(drops("(A&B)|array $x", ""), exit(&["~A", "~B"]));
    // `self` and `parent` are the enclosing class and its parent, which the resolver reads.
    assert_eq!(drops("self $x", ""), exit(&["self"]));
    assert_eq!(drops("?self|array $x", ""), exit(&["self"]));
    assert_eq!(drops("parent $x", ""), exit(&["parent"]));
    // A repeated class is one receiver.
    assert_eq!(drops("D|?D $x", ""), exit(&["~D"]));
}

#[test]
fn a_parameter_that_cannot_name_a_class_or_is_not_dropped_by_value_is_no_subject() {
    for params in [
        "int $i",
        "array $a",
        "mixed $m",
        "iterable $i",
        "callable $c",
        "object $o",
        "$u",
        "string|int $s",
        "array|null $a",
        "&$r",
        "D &$r",
        "D ...$ds",
    ] {
        assert_eq!(drops(params, "").len(), 0, "{params}");
    }
    // A promoted parameter lives on in its property.
    let src = "<?php\nclass K { public function __construct(private D $d) {} }\n";
    let tree = SourceTree::parse(src);
    let sites = &tree.classes()[0].methods[0].sites;
    assert!(sites.iter().all(|s| !matches!(s.kind, SiteKind::Operator { family: F::Drop, .. })));
}

#[test]
fn a_new_temporary_is_a_site_in_statement_receiver_and_argument_position() {
    assert_eq!(drops("", "new D;"), [drop_site(C::DropTemporary, &["D"], "new D")]);
    assert_eq!(drops("", "(new R)->m();"), [drop_site(C::DropTemporary, &["R"], "new R")]);
    assert_eq!(drops("", "foo(new D, 1);"), [drop_site(C::DropTemporary, &["D"], "new D")]);
    assert_eq!(drops("", "$o->m(x: new D());"), [drop_site(C::DropTemporary, &["D"], "new D()")]);
    assert_eq!(drops("", "K::m(new D);"), [drop_site(C::DropTemporary, &["D"], "new D")]);
    assert_eq!(
        drops("", "new X(new D);"),
        [
            drop_site(C::DropTemporary, &["X"], "new X(new D)"),
            drop_site(C::DropTemporary, &["D"], "new D"),
        ]
    );
    assert_eq!(drops("", "$x = foo(new D);"), [drop_site(C::DropTemporary, &["D"], "new D")]);
}

#[test]
fn a_new_that_escapes_is_no_temporary() {
    for body in [
        "$x = new D;",
        "return new D;",
        "$this->d = new D;",
        "yield new D;",
        "$a = [new D];",
        "$c = function () { return new D; };",
        "echo new D;",
        "foo(...[new D]);",
        "$o = new $c;",
        "new static;",
    ] {
        let sites = drops("", body);
        assert!(!sites.iter().any(|s| s.0 == C::DropTemporary), "{body}: {sites:?}");
    }
}

#[test]
fn an_anonymous_class_carries_what_its_body_declares() {
    // A destructor of its own, or an imported method aliased to one: user code at the drop.
    for class in ["{ function __destruct() {} }", "{ use T { bye as __destruct; } }"] {
        let body = format!("$x = new class {class}; unset($x);");
        assert_eq!(
            drops("", &body),
            [drop_site(C::DropUnset, &["?"], "$x"), drop_site(C::DropScopeExit, &["?"], "}")],
            "{class}"
        );
    }
    // A trait it imports is read as a class by the resolver, beside its parent.
    assert_eq!(
        drops("", "$x = new class extends P { use T, U; }; unset($x);"),
        [
            drop_site(C::DropUnset, &["T", "U", "P"], "$x"),
            drop_site(C::DropScopeExit, &["T", "U", "P"], "}"),
        ]
    );
    // A parent is the class for the chain's sake; no parent and no destructor runs nothing.
    assert_eq!(
        drops("", "new class extends P {};"),
        [drop_site(C::DropTemporary, &["P"], "new class extends P {}")]
    );
    assert_eq!(drops("", "new class {}; $x = new class {}; unset($x);"), []);
    assert_eq!(
        drops("", "foo(new class { function __destruct() {} });"),
        [drop_site(C::DropTemporary, &["?"], "new class { function __destruct() {} }")]
    );
}

#[test]
fn a_closure_has_its_own_subjects() {
    let src = "<?php\nfunction f() { $g = function () { $x = new D; }; $x = new E; }\n";
    let tree = SourceTree::parse(src);
    let sites = &tree.functions()[0].sites;
    let drop_exits = |sites: &[steins_syntax::SiteOrigin]| {
        sites
            .iter()
            .filter(|s| {
                matches!(s.kind, SiteKind::Operator { construct: C::DropScopeExit, .. })
            })
            .count()
    };
    // The frame holds `$x = new E` only; the closure's `$x` is the closure's.
    assert_eq!(drop_exits(sites), 1);
    let closure = tree
        .scopes()
        .iter()
        .find(|s| matches!(s.owner, steins_syntax::ScopeOwner::Closure { .. }))
        .expect("a closure scope");
    assert_eq!(drop_exits(&closure.sites), 1);
}

#[test]
fn drop_sites_are_neither_lanes_origins() {
    let src = "<?php\nfunction f(D $d) { $x = new E; unset($x); new F; }\n";
    let tree = SourceTree::parse(src);
    let sites = &tree.functions()[0].sites;
    assert!(sites.iter().any(|s| matches!(s.kind, SiteKind::Operator { family: F::Drop, .. })));
    let origins = format!("{:?}", derive_effect_origins(sites));
    assert!(!origins.contains("Operator") && !origins.contains("Drop"), "{origins}");
    assert!(!format!("{:?}", derive_throw_origins(sites)).contains("Drop"));
}
