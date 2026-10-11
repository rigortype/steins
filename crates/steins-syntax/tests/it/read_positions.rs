//! The read positions a statement carries (ADR-0104, issue #1050): which calls, offset
//! reads, property reads and class-constant reads its own expressions write, at which span,
//! and under which carrier.
//!
//! The IR has no consumer yet, so these pin the lowering itself: every [`Carrier`], and
//! every boundary the scan stops at.

use steins_syntax::{Carrier, ReadKind, ReadPosition, SourceTree, Stmt};

fn render_carrier(c: &Carrier) -> String {
    match c {
        Carrier::Plain => "plain".to_owned(),
        Carrier::Argument { callee, position } => format!(
            "arg({},{})",
            callee.as_ref().map_or("-", |c| c.raw.as_str()),
            position.map_or("-".to_owned(), |p| p.to_string())
        ),
        Carrier::Conditional => "cond".to_owned(),
        Carrier::IssetLike => "isset".to_owned(),
        Carrier::Suppressed => "suppressed".to_owned(),
        Carrier::Write => "write".to_owned(),
        Carrier::CompoundRead => "compound".to_owned(),
        Carrier::FirstClassCallable => "fcc".to_owned(),
    }
}

/// `"<source text of the read> [<carrier>]"` for each position, in the order recorded.
fn render(src: &str, positions: &[ReadPosition]) -> Vec<String> {
    positions
        .iter()
        .map(|p| {
            let text = &src[p.span.start as usize..p.span.end as usize];
            format!("{text} [{}]", render_carrier(&p.carrier))
        })
        .collect()
}

/// Every statement of `stmts` and of the sub-traces under it, outermost first.
fn flatten<'a>(stmts: &'a [Stmt], out: &mut Vec<&'a Stmt>) {
    for s in stmts {
        out.push(s);
        for sub in s.kind.sub_traces() {
            flatten(sub, out);
        }
    }
}

/// The positions of every statement of each scope, scope by scope in the tree's order.
fn by_scope(src: &str) -> Vec<Vec<String>> {
    let tree = SourceTree::parse(src);
    tree.scopes()
        .iter()
        .map(|sc| {
            let mut all = Vec::new();
            flatten(&sc.stmts, &mut all);
            all.iter().flat_map(|s| render(src, s.read_positions())).collect()
        })
        .collect()
}

/// The positions of `body` as the statements of `function f($a, $o, $x) { body }`.
fn reads(body: &str) -> Vec<String> {
    let src = format!("<?php\nclass C {{ const B = 1; public function m() {{}} }}\nfunction f($a, $o, $x) {{\n{body}\n}}\n");
    let tree = SourceTree::parse(&src);
    let f = tree
        .scopes()
        .iter()
        .find(|sc| matches!(&sc.owner, steins_syntax::ScopeOwner::Function { name: n, .. } if n == "f"))
        .expect("the function's scope");
    let mut all = Vec::new();
    flatten(&f.stmts, &mut all);
    all.iter().flat_map(|s| render(&src, s.read_positions())).collect()
}

#[test]
fn an_operand_is_plain() {
    assert_eq!(reads("echo $a['k'] + 1;"), ["$a['k'] [plain]"]);
    assert_eq!(reads("return $o->p . 'x';"), ["$o->p [plain]"]);
    assert_eq!(reads("$r = A::B;"), ["A::B [plain]"]);
}

#[test]
fn a_chain_records_every_level_and_stops_at_a_receiver_that_is_not_a_variable() {
    assert_eq!(reads("echo $a['n']['zz'];"), ["$a['n']['zz'] [plain]", "$a['n'] [plain]"]);
    assert_eq!(reads("echo $o->a->b;"), ["$o->a [plain]"]);
    assert_eq!(reads("echo $o?->p;"), Vec::<String>::new());
}

#[test]
fn class_constant_class_keyword_and_dynamic_class_are_not_positions() {
    assert_eq!(reads("echo C::class;"), Vec::<String>::new());
    assert_eq!(reads("echo $o::B;"), Vec::<String>::new());
}

#[test]
fn an_argument_carries_its_callee_and_position() {
    assert_eq!(
        reads("$r = strlen($a['k']);"),
        ["strlen($a['k']) [plain]", "$a['k'] [arg(strlen,0)]"]
    );
    assert_eq!(
        reads("f2(1, $a['k']);"),
        ["f2(1, $a['k']) [plain]", "$a['k'] [arg(f2,1)]"]
    );
    // A method, a static call and a constructor name no function.
    assert_eq!(reads("$o->m($a['k']);"), ["$o->m($a['k']) [plain]", "$a['k'] [arg(-,0)]"]);
    assert_eq!(reads("C::m($a['k']);"), ["C::m($a['k']) [plain]", "$a['k'] [arg(-,0)]"]);
    assert_eq!(reads("$r = new C($a['k']);"), ["new C($a['k']) [plain]", "$a['k'] [arg(-,0)]"]);
}

#[test]
fn a_named_argument_and_one_after_a_spread_have_no_position() {
    assert_eq!(
        reads("f2(k: $a['k']);"),
        ["f2(k: $a['k']) [plain]", "$a['k'] [arg(f2,-)]"]
    );
    // The spread operand is read whole; the argument after it is not at a known position.
    assert_eq!(
        reads("f2(...$a['k'], $a['j']);"),
        ["f2(...$a['k'], $a['j']) [plain]", "$a['k'] [plain]", "$a['j'] [arg(f2,-)]"]
    );
}

#[test]
fn the_base_of_an_argument_chain_inherits_the_argument() {
    assert_eq!(
        reads("f2($a['n']['zz']);"),
        ["f2($a['n']['zz']) [plain]", "$a['n']['zz'] [arg(f2,0)]", "$a['n'] [arg(f2,0)]"]
    );
}

#[test]
fn a_ternary_arm_and_a_short_circuit_operand_are_conditional() {
    assert_eq!(
        reads("$r = $x ? $a['k'] : $a['j'];"),
        ["$a['k'] [cond]", "$a['j'] [cond]"]
    );
    assert_eq!(reads("$r = $x && $a['k'];"), ["$a['k'] [cond]"]);
    assert_eq!(reads("$r = $a['k'] || $a['j'];"), ["$a['k'] [plain]", "$a['j'] [cond]"]);
    assert_eq!(reads("$r = $a['k'] and $a['j'];"), ["$a['k'] [plain]", "$a['j'] [cond]"]);
}

#[test]
fn a_ternary_test_and_the_left_of_an_elvis_are_plain() {
    assert_eq!(reads("$r = $a['t'] ? 1 : 2;"), ["$a['t'] [plain]"]);
    assert_eq!(reads("$r = $a['k'] ?: $a['j'];"), ["$a['k'] [plain]", "$a['j'] [cond]"]);
}

#[test]
fn a_call_in_a_conditional_position_is_conditional() {
    assert_eq!(reads("$r = $x && nope_fn();"), ["nope_fn() [cond]"]);
    assert_eq!(
        reads("$r = $x ? strlen($a['k']) : 0;"),
        ["strlen($a['k']) [cond]", "$a['k'] [cond]"]
    );
}

#[test]
fn isset_empty_and_the_left_of_coalesce_are_issetlike() {
    assert_eq!(
        reads("$r = isset($a['k']['j']);"),
        ["$a['k']['j'] [isset]", "$a['k'] [isset]"]
    );
    assert_eq!(reads("$r = empty($o->p);"), ["$o->p [isset]"]);
    assert_eq!(reads("$r = $a['k'] ?? $a['j'];"), ["$a['k'] [isset]", "$a['j'] [cond]"]);
    assert_eq!(reads("$a['k'] ??= $a['j'];"), ["$a['k'] [isset]", "$a['j'] [cond]"]);
    // A class constant is a position under `??` too: PHP throws there, so the carrier says
    // where it sits and the family decides.
    assert_eq!(reads("$r = C::B ?? 1;"), ["C::B [isset]"]);
}

#[test]
fn a_key_inside_an_isset_is_an_ordinary_read() {
    assert_eq!(
        reads("$r = isset($a[$x['i']]);"),
        ["$a[$x['i']] [isset]", "$x['i'] [plain]"]
    );
}

#[test]
fn an_at_operand_is_suppressed() {
    assert_eq!(reads("$r = @$a['k'];"), ["$a['k'] [suppressed]"]);
    // The weakest claim wins: the argument is under `@` as well.
    assert_eq!(
        reads("$r = @strlen($a['k']);"),
        ["strlen($a['k']) [suppressed]", "$a['k'] [suppressed]"]
    );
    // A conditional operand stays conditional under `@`.
    assert_eq!(reads("$r = @($x && $a['k']);"), ["$a['k'] [cond]"]);
}

#[test]
fn a_target_is_a_write() {
    assert_eq!(reads("$a['k'] = 1;"), ["$a['k'] [write]"]);
    assert_eq!(reads("$o->p = 1;"), ["$o->p [write]"]);
    assert_eq!(reads("$a['n']['k'] = 1;"), ["$a['n']['k'] [write]", "$a['n'] [write]"]);
    assert_eq!(reads("$r = &$a['k'];"), ["$a['k'] [write]"]);
    assert_eq!(reads("$r = f2(...[&$a['k']]);"), ["f2(...[&$a['k']]) [plain]", "$a['k'] [write]"]);
}

#[test]
fn the_key_of_a_target_and_the_value_assigned_are_reads() {
    assert_eq!(reads("$a[$x['i']] = $o->p;"), ["$a[$x['i']] [write]", "$x['i'] [plain]", "$o->p [plain]"]);
}

#[test]
fn a_destructuring_target_is_a_write_and_its_key_a_read() {
    assert_eq!(reads("[$o->p, $a['k']] = $x;"), ["$o->p [write]", "$a['k'] [write]"]);
    assert_eq!(reads("['i' => $a['k']] = $x;"), ["$a['k'] [write]"]);
    assert_eq!(reads("[$a[$x['i']]] = f2();"), ["$a[$x['i']] [write]", "$x['i'] [plain]", "f2() [plain]"]);
}

#[test]
fn a_compound_target_and_an_increment_are_compound_reads() {
    assert_eq!(reads("$a['k'] .= 'x';"), ["$a['k'] [compound]"]);
    assert_eq!(reads("$a['n']['k'] += 1;"), ["$a['n']['k'] [compound]", "$a['n'] [write]"]);
    assert_eq!(reads("$o->n++;"), ["$o->n [compound]"]);
    assert_eq!(reads("--$a['k'];"), ["$a['k'] [compound]"]);
}

#[test]
fn a_first_class_callable_is_its_own_carrier() {
    assert_eq!(reads("$r = strlen(...);"), ["strlen(...) [fcc]"]);
    assert_eq!(reads("$r = $o->m(...);"), ["$o->m(...) [fcc]"]);
    assert_eq!(reads("$r = C::m(...);"), ["C::m(...) [fcc]"]);
    // Whatever surrounds it: it evaluates no call.
    assert_eq!(reads("$r = $x ? strlen(...) : 0;"), ["strlen(...) [fcc]"]);
}

#[test]
fn echo_return_and_a_static_initializer_are_read() {
    assert_eq!(reads("echo $a['k'], $o->p;"), ["$a['k'] [plain]", "$o->p [plain]"]);
    assert_eq!(reads("return f2($a['k']);"), ["f2($a['k']) [plain]", "$a['k'] [arg(f2,0)]"]);
    assert_eq!(
        reads("static $s = f2($a['k']);"),
        ["f2($a['k']) [plain]", "$a['k'] [arg(f2,0)]"]
    );
}

#[test]
fn a_nested_call_is_a_position_at_its_own_span() {
    assert_eq!(
        reads("echo f2(g2(nope_fn()));"),
        ["f2(g2(nope_fn())) [plain]", "g2(nope_fn()) [arg(f2,0)]", "nope_fn() [arg(g2,0)]"]
    );
    assert_eq!(
        reads("$r = [nope_fn() => 1];"),
        ["nope_fn() [plain]"]
    );
    assert_eq!(reads("throw new E(nope_fn());"), ["new E(nope_fn()) [plain]", "nope_fn() [arg(-,0)]"]);
}

#[test]
fn a_header_is_not_read() {
    assert_eq!(reads("if ($a['k']) { }"), Vec::<String>::new());
    assert_eq!(reads("while ($a['k']) { break; }"), Vec::<String>::new());
    assert_eq!(reads("foreach ($a['k'] as $v) { }"), Vec::<String>::new());
    // The body's statements carry their own.
    assert_eq!(reads("if ($x) { echo $a['k']; }"), ["$a['k'] [plain]"]);
}

#[test]
fn a_closure_body_is_its_own_scope() {
    let src = "<?php\nfunction f($a) {\n$g = function () use ($a) { return $a['k']; };\n}\n";
    let got = by_scope(src);
    // The enclosing scope reads nothing; the closure's own statement reads `$a['k']`.
    assert_eq!(got.iter().filter(|s| !s.is_empty()).count(), 1, "{got:?}");
    assert!(got.iter().any(|s| s == &["$a['k'] [plain]"]), "{got:?}");
}

#[test]
fn an_arrow_function_body_is_its_own_scope() {
    let src = "<?php\nfunction f($a) {\n$g = fn () => $a['k'];\n}\n";
    let got = by_scope(src);
    assert_eq!(got.iter().filter(|s| !s.is_empty()).count(), 1, "{got:?}");
    assert!(got.iter().any(|s| s == &["$a['k'] [plain]"]), "{got:?}");
}

#[test]
fn a_property_hook_body_is_its_own_scope() {
    let src = "<?php\nclass K { public int $h { get => $this->q['z']; } }\n";
    let got = by_scope(src);
    assert!(got.iter().any(|s| s == &["$this->q['z'] [plain]", "$this->q [plain]"]), "{got:?}");
    // The hook does not leak into a statement around it.
    let src = "<?php\nfunction f() { $x = new class { public int $h { get => $this->q['z']; } }; }\n";
    let got = by_scope(src);
    assert!(got.iter().all(Vec::is_empty), "{got:?}");
}

#[test]
fn an_anonymous_class_contributes_its_constructor_arguments_and_nothing_of_its_body() {
    let src = "<?php\nfunction f($a) {\n$x = new class($a['k']) { public function g() { return $this->p['k']; } };\n}\n";
    let got = by_scope(src);
    let f = got.iter().find(|s| s.iter().any(|t| t.starts_with("$a['k']"))).expect("f's scope");
    assert_eq!(f, &["$a['k'] [arg(-,0)]"], "{got:?}");
}

#[test]
fn a_structured_match_is_not_descended_and_its_arms_carry_their_own() {
    let src = "<?php\nfunction f($a, $x) {\n$r = match ($x) { 1 => $a['k'], default => 2 };\n}\n";
    let got = by_scope(src);
    // One position in the whole scope, recorded once, at the arm.
    let all: Vec<&String> = got.iter().flatten().collect();
    assert_eq!(all, ["$a['k'] [plain]"], "{got:?}");
}

#[test]
fn a_hoisted_match_arm_is_read_as_a_statement_of_its_own() {
    let src = "<?php\nfunction f($a, $x) {\nreturn match ($x) { 1 => nope_fn(), default => $a['k'] };\n}\n";
    let got = by_scope(src);
    let all: Vec<&String> = got.iter().flatten().collect();
    assert_eq!(all, ["nope_fn() [plain]", "$a['k'] [plain]"], "{got:?}");
}

#[test]
fn a_position_records_the_read_and_not_the_statement() {
    let src = "<?php\nfunction f($a) {\n    echo 'x' . $a['k'];\n}\n";
    let tree = SourceTree::parse(src);
    let f = tree.scopes().iter().find(|sc| sc.stmts.iter().any(|s| !s.read_positions().is_empty()));
    let stmt = &f.expect("the scope with a read").stmts[0];
    let pos = &stmt.read_positions()[0];
    assert!(matches!(pos.kind, ReadKind::Offset { .. }), "{pos:?}");
    assert!(pos.span.start > stmt.span.start && pos.span.end < stmt.span.end, "{pos:?} in {:?}", stmt.span);
}

#[test]
fn an_empty_statement_costs_one_word() {
    let src = "<?php\nfunction f($a) { $b = 1; }\n";
    let tree = SourceTree::parse(src);
    let f = tree.scopes().iter().find(|sc| !sc.stmts.is_empty()).expect("scope");
    assert!(f.stmts.iter().all(|s| s.read_positions.is_none()));
}
