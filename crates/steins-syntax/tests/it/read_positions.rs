//! The read positions a statement carries (ADR-0104, issue #1050): which calls, offset
//! reads, property reads and class-constant reads its own expressions write, at which span,
//! and under which carrier.
//!
//! The IR has no consumer yet, so these pin the lowering itself: every [`Carrier`], and
//! every boundary the scan stops at.

use steins_syntax::{Carrier, ReadKind, ReadPosition, SourceTree, Stmt};

/// The carrier as a word; an argument names its call by the text before the `(` of the call
/// position it points at (`strlen`, `$o->m`, `new C`).
fn render_carrier(src: &str, all: &[ReadPosition], c: &Carrier) -> String {
    match c {
        Carrier::Plain => "plain".to_owned(),
        Carrier::Argument { call, position } => {
            let callee = call.map_or("-", |i| {
                let s = all[i as usize].span;
                src[s.start as usize..s.end as usize].split('(').next().unwrap_or("")
            });
            format!("arg({callee},{})", position.map_or("-".to_owned(), |p| p.to_string()))
        }
        Carrier::Conditional => "cond".to_owned(),
        Carrier::IssetLike => "isset".to_owned(),
        Carrier::Suppressed => "suppressed".to_owned(),
        Carrier::CompoundRead => "compound".to_owned(),
    }
}

/// `"<source text of the read> [<carrier>]"` for each position, in the order recorded.
fn render(src: &str, positions: &[ReadPosition]) -> Vec<String> {
    positions
        .iter()
        .map(|p| {
            let text = &src[p.span.start as usize..p.span.end as usize];
            let carrier = render_carrier(src, positions, &p.carrier);
            // A first-class callable is its own kind; its carrier folds like a call's.
            match (&p.kind, p.carrier) {
                (ReadKind::CallRef, Carrier::Plain) => format!("{text} [fcc]"),
                (ReadKind::CallRef, _) => format!("{text} [fcc,{carrier}]"),
                _ => format!("{text} [{carrier}]"),
            }
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
    assert_eq!(reads("$o->m($a['k']);"), ["$o->m($a['k']) [plain]", "$a['k'] [arg($o->m,0)]"]);
    assert_eq!(reads("C::m($a['k']);"), ["C::m($a['k']) [plain]", "$a['k'] [arg(C::m,0)]"]);
    assert_eq!(reads("$r = new C($a['k']);"), ["new C($a['k']) [plain]", "$a['k'] [arg(new C,0)]"]);
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
    // Every family is silent on an offset or a property there: not recorded at all.
    assert_eq!(reads("$r = isset($a['k']['j']);"), Vec::<String>::new());
    assert_eq!(reads("$r = empty($o->p);"), Vec::<String>::new());
    assert_eq!(reads("$r = $a['k'] ?? $a['j'];"), ["$a['j'] [cond]"]);
    assert_eq!(reads("$a['k'] ??= $a['j'];"), ["$a['j'] [cond]"]);
    // A class constant is a position under `??` too: PHP throws there, so the carrier says
    // where it sits and the family decides.
    assert_eq!(reads("$r = C::B ?? 1;"), ["C::B [isset]"]);
}

#[test]
fn a_key_inside_an_isset_is_an_ordinary_read() {
    assert_eq!(
        reads("$r = isset($a[$x['i']]);"),
        ["$x['i'] [plain]"]
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
    // A write is not recorded: every family is silent on it.
    let none = Vec::<String>::new();
    assert_eq!(reads("$a['k'] = 1;"), none);
    assert_eq!(reads("$o->p = 1;"), none);
    assert_eq!(reads("$a['n']['k'] = 1;"), none);
    assert_eq!(reads("$r = &$a['k'];"), none);
    assert_eq!(reads("$r = f2(...[&$a['k']]);"), ["f2(...[&$a['k']]) [plain]"]);
}

#[test]
fn the_key_of_a_target_and_the_value_assigned_are_reads() {
    assert_eq!(reads("$a[$x['i']] = $o->p;"), ["$x['i'] [plain]", "$o->p [plain]"]);
}

#[test]
fn a_destructuring_target_is_a_write_and_its_key_a_read() {
    assert_eq!(reads("[$o->p, $a['k']] = $x;"), Vec::<String>::new());
    assert_eq!(reads("['i' => $a['k']] = $x;"), Vec::<String>::new());
    assert_eq!(reads("[$a[$x['i']]] = f2();"), ["$x['i'] [plain]", "f2() [plain]"]);
}

#[test]
fn a_compound_target_and_an_increment_are_compound_reads() {
    assert_eq!(reads("$a['k'] .= 'x';"), ["$a['k'] [compound]"]);
    assert_eq!(reads("$a['n']['k'] += 1;"), ["$a['n']['k'] [compound]"]);
    assert_eq!(reads("$o->n++;"), ["$o->n [compound]"]);
    assert_eq!(reads("--$a['k'];"), ["$a['k'] [compound]"]);
}

#[test]
fn a_first_class_callable_is_its_own_carrier() {
    assert_eq!(reads("$r = strlen(...);"), ["strlen(...) [fcc]"]);
    assert_eq!(reads("$r = $o->m(...);"), ["$o->m(...) [fcc]"]);
    assert_eq!(reads("$r = C::m(...);"), ["C::m(...) [fcc]"]);
    // Whatever surrounds it: it evaluates no call.
    assert_eq!(reads("$r = $x ? strlen(...) : 0;"), ["strlen(...) [fcc,cond]"]);
    assert_eq!(reads("$r = $x && missing(...);"), ["missing(...) [fcc,cond]"]);
    assert_eq!(reads("$r = @missing(...);"), ["missing(...) [fcc,suppressed]"]);
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
    assert_eq!(reads("throw new E(nope_fn());"), ["new E(nope_fn()) [plain]", "nope_fn() [arg(new E,0)]"]);
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
    assert!(matches!(pos.kind, ReadKind::Offset(_)), "{pos:?}");
    assert!(pos.span.start > stmt.span.start && pos.span.end < stmt.span.end, "{pos:?} in {:?}", stmt.span);
}

#[test]
fn the_base_of_an_append_is_written_not_read() {
    let none = Vec::<String>::new();
    assert_eq!(reads("$a['k'][] = 1;"), none);
    assert_eq!(reads("$a['x']['y'][] = 1;"), none);
    assert_eq!(reads("$o->p[] = 3;"), none);
    assert_eq!(reads("[$a['x'][]] = [1];"), none);
    // The key operands inside the base are ordinary reads.
    assert_eq!(reads("$a[$x['i']][] = 1;"), ["$x['i'] [plain]"]);
}

#[test]
fn nothing_after_a_nullsafe_is_unconditional() {
    // Arguments of a `?->` call.
    assert_eq!(
        reads("$o?->m(missing(), A::NOPE, $a['k']);"),
        [
            "$o?->m(missing(), A::NOPE, $a['k']) [cond]",
            "missing() [cond]",
            "A::NOPE [cond]",
            "$a['k'] [cond]",
        ]
    );
    // The key of an access past a `?->`, and the access itself.
    assert_eq!(
        reads("echo $o?->p[$a['nm']];"),
        ["$o?->p[$a['nm']] [cond]", "$a['nm'] [cond]"]
    );
    // Every later link of the chain, and its arguments.
    assert_eq!(
        reads("$o?->m()->q($a['chain']);"),
        ["$o?->m()->q($a['chain']) [cond]", "$o?->m() [cond]", "$a['chain'] [cond]"]
    );
    // The receiver itself is evaluated.
    assert_eq!(reads("$a['r']?->m();"), ["$a['r']?->m() [cond]", "$a['r'] [plain]"]);
    // Under `??` the access is silent and not recorded.
    assert_eq!(reads("$r = $o?->p['nsidx'] ?? 0;"), Vec::<String>::new());
}

#[test]
fn a_nullsafe_in_a_static_call_class_operand_short_circuits_it() {
    assert_eq!(
        reads("$o?->s::m($a['k6']);"),
        ["$o?->s::m($a['k6']) [cond]", "$a['k6'] [cond]"]
    );
    assert_eq!(
        reads("($o?->p)::m($a['k5']);"),
        ["($o?->p)::m($a['k5']) [cond]", "$a['k5'] [cond]"]
    );
    // Without a `?->` the same shape is unconditional.
    assert_eq!(
        reads("$o->s::m($a['k']);"),
        ["$o->s::m($a['k']) [plain]", "$o->s [plain]", "$a['k'] [arg($o->s::m,0)]"]
    );
}

#[test]
fn unset_is_not_a_scanned_statement() {
    assert_eq!(reads("unset($a['k'][$x['i']]);"), Vec::<String>::new());
}

#[test]
fn a_match_subject_is_an_operand_and_its_arm_conditions_are_not_read_here() {
    // S4: the subject is evaluated as an operand of the statement; the arm conditions are
    // the structured construct's headers.
    assert_eq!(
        reads("echo match ($a['subj']) { $a['arm'] => 1, default => 2 };"),
        ["$a['subj'] [plain]"]
    );
    assert_eq!(reads("$r = $x ? match (C::B) { default => 2 } : 0;"), ["C::B [cond]"]);
}

/// The positions of every header of `body`'s statements and their sub-traces, in the
/// order the constructs list them.
fn header_reads(body: &str) -> Vec<String> {
    use steins_syntax::{Header, StmtKind};
    let src = format!("<?php\nclass C {{ const B = 1; }}\nfunction f($a, $o, $x) {{\n{body}\n}}\n");
    let tree = SourceTree::parse(&src);
    let f = tree
        .scopes()
        .iter()
        .find(|sc| matches!(&sc.owner, steins_syntax::ScopeOwner::Function { name: n, .. } if n == "f"))
        .expect("the function's scope");
    let mut all = Vec::new();
    flatten(&f.stmts, &mut all);
    let mut heads: Vec<&Header> = Vec::new();
    for s in all {
        match &s.kind {
            StmtKind::If { heads: h, .. } => heads.extend(h),
            StmtKind::While { head, .. }
            | StmtKind::Foreach { head, .. }
            | StmtKind::DoWhile { head, .. } => heads.push(head),
            StmtKind::For { head, step, .. } => heads.extend([head, step]),
            StmtKind::Match { head, arms, .. } => {
                heads.push(head);
                heads.extend(arms.iter().map(|a| &a.head));
            }
            _ => {}
        }
    }
    heads
        .iter()
        .filter_map(|h| h.eval.as_deref())
        .flat_map(|st| render(&src, st.read_positions()))
        .collect()
}

#[test]
fn every_condition_of_an_if_chain_is_a_header() {
    assert_eq!(
        header_reads("if (f1()) {} elseif ($a['k'] && f2()) {} else {}"),
        ["f1() [plain]", "$a['k'] [plain]", "f2() [cond]"]
    );
    // The statement itself records none of them.
    assert_eq!(reads("if (f1()) {} elseif ($a['k']) {}"), Vec::<String>::new());
}

#[test]
fn loop_headers_are_headers() {
    assert_eq!(header_reads("while ($a['k']) {}"), ["$a['k'] [plain]"]);
    assert_eq!(header_reads("foreach (f1() as $v) {}"), ["f1() [plain]"]);
    // A `for` step and a `do`-`while` condition run after an iteration completes.
    assert_eq!(
        header_reads("for ($i = f0(); f1(), $a['k']; f2($a['s'])) {}"),
        ["f1() [plain]", "$a['k'] [plain]", "f2($a['s']) [cond]", "$a['s'] [cond]"]
    );
    assert_eq!(header_reads("do {} while (f1() ?? $a['k']);"), ["f1() [cond]", "$a['k'] [cond]"]);
}

#[test]
fn switch_and_match_headers() {
    assert_eq!(
        header_reads("switch ($a['k']) { case C::B: break; }"),
        ["$a['k'] [plain]", "C::B [plain]"]
    );
    assert_eq!(
        header_reads("switch (true) { case f1(): case f2(): break; }"),
        ["f1() [plain]", "f2() [plain]"]
    );
    // A statement-position `match`: the subject is the statement's, an arm condition a
    // header.
    assert_eq!(header_reads("match ($x) { C::B => 1, default => 2 };"), ["C::B [plain]"]);
    assert_eq!(header_reads("match (true) { f1() => 1, default => 2 };"), ["f1() [plain]"]);
    // A hoisted one is walked ahead of its statement: `Conditional`.
    assert_eq!(header_reads("return match (true) { f1() => 1, default => 2 };"), ["f1() [cond]"]);
}

#[test]
fn a_position_stays_small() {
    // 8 (span) + 16 (carrier) + 16 (kind) at the time of writing; the bound is the contract.
    let size = std::mem::size_of::<ReadPosition>();
    assert!(size <= 40, "a ReadPosition grew to {size} bytes");
}

#[test]
fn an_offset_names_its_place_and_literal_key() {
    use steins_syntax::{Key, Place};
    let src = "<?php\nfunction f($a) { echo $a['n'][3]; }\n";
    let tree = SourceTree::parse(src);
    let f = tree.scopes().iter().find(|sc| sc.stmts.iter().any(|s| !s.read_positions().is_empty()));
    let positions = f.expect("scope").stmts[0].read_positions();
    let ReadKind::Offset(outer) = &positions[0].kind else { panic!("{positions:?}") };
    assert_eq!(outer.1, Key::Int(3));
    let Place::Index(inner) = &outer.0 else { panic!("{outer:?}") };
    assert_eq!(inner.0, Place::Var("a".to_owned()));
    assert!(matches!(&inner.1, Key::Str(_)), "{inner:?}");
}

#[test]
fn an_offset_names_a_variable_key() {
    use steins_syntax::{Key, Place};
    let src = "<?php\nfunction f($a, $k) { echo $a[$k]['x'], $a[$k . 'y']; }\n";
    let tree = SourceTree::parse(src);
    let f = tree.scopes().iter().find(|sc| sc.stmts.iter().any(|s| !s.read_positions().is_empty()));
    let positions = f.expect("scope").stmts[0].read_positions();
    let ReadKind::Offset(outer) = &positions[0].kind else { panic!("{positions:?}") };
    let Place::Index(inner) = &outer.0 else { panic!("{outer:?}") };
    assert_eq!(inner.1, Key::Var("k".to_owned()));
    let ReadKind::Offset(computed) = &positions[2].kind else { panic!("{positions:?}") };
    assert_eq!(computed.1, Key::Other);
}

#[test]
fn an_empty_statement_costs_one_word() {
    let src = "<?php\nfunction f($a) { $b = 1; }\n";
    let tree = SourceTree::parse(src);
    let f = tree.scopes().iter().find(|sc| !sc.stmts.is_empty()).expect("scope");
    assert!(f.stmts.iter().all(|s| s.read_positions.is_none()));
}

/// For each call or first-class-callable position of `body`, its text and where its lowered
/// call is found: `stmt` (`Stmt::read_calls`: a `new`, a function callable), `tree`
/// (`SourceTree::call_at`: a function call), `scope` (`Scope::method_call_at`: a method or
/// static call or callable), or `none`.
fn resolved(body: &str) -> Vec<String> {
    let src = format!("<?php\nclass C {{ public function m() {{}} }}\nfunction f($a, $o, $x) {{\n{body}\n}}\n");
    let tree = SourceTree::parse(&src);
    let f = tree
        .scopes()
        .iter()
        .find(|sc| matches!(&sc.owner, steins_syntax::ScopeOwner::Function { name: n, .. } if n == "f"))
        .expect("the function's scope");
    let mut all = Vec::new();
    flatten(&f.stmts, &mut all);
    let mut out = Vec::new();
    for s in all {
        for p in s.read_positions() {
            if !matches!(p.kind, ReadKind::Call | ReadKind::CallRef) {
                continue;
            }
            let found = if s.read_calls().iter().any(|c| c.span == p.span) {
                "stmt"
            } else if tree.call_at(p.span).is_some() {
                "tree"
            } else if f.method_call_at(p.span).is_some() {
                "scope"
            } else {
                "none"
            };
            out.push(format!("{} -> {found}", &src[p.span.start as usize..p.span.end as usize]));
        }
    }
    out
}

#[test]
fn every_call_position_finds_its_lowered_call_in_exactly_one_list() {
    assert_eq!(resolved("echo g(f($a));"), ["g(f($a)) -> tree", "f($a) -> tree"]);
    assert_eq!(resolved("$r = g($o->m());"), ["g($o->m()) -> tree", "$o->m() -> scope"]);
    assert_eq!(resolved("echo C::s(1);"), ["C::s(1) -> scope"]);
    assert_eq!(resolved("throw new C(f($a));"), ["new C(f($a)) -> stmt", "f($a) -> tree"]);
    assert_eq!(resolved("echo $o?->m();"), ["$o?->m() -> scope"]);
}

#[test]
fn a_first_class_callable_is_found_by_its_kind() {
    assert_eq!(resolved("$k = f(...);"), ["f(...) -> stmt"]);
    assert_eq!(resolved("$k = $o->m(...);"), ["$o->m(...) -> scope"]);
    assert_eq!(resolved("$k = C::m(...);"), ["C::m(...) -> scope"]);
}

#[test]
fn a_computed_callee_and_a_dynamic_class_have_a_position_and_no_call() {
    assert_eq!(resolved("echo $x();"), ["$x() -> tree"]);
    assert_eq!(resolved("echo $o->$a();"), ["$o->$a() -> scope"]);
    assert_eq!(resolved("$k = new $x();"), ["new $x() -> none"]);
}

#[test]
fn the_tree_and_the_scope_list_their_calls_in_source_order() {
    // `call_at` bisects on the start offset, which only holds if the lists are in source
    // order with a call before the calls inside it.
    let src = "<?php\nfunction f($o) { return g(h($o->a($o->b()), $o->c())) . $o->d()(1); }\nfunction k() { return f(1)(2); }\n";
    let tree = SourceTree::parse(src);
    let starts: Vec<u32> = tree.calls().iter().map(|c| c.span.start).collect();
    assert!(starts.windows(2).all(|w| w[0] <= w[1]), "{starts:?}");
    for sc in tree.scopes() {
        let starts: Vec<u32> = sc.method_calls.iter().map(|c| c.span.start).collect();
        assert!(starts.windows(2).all(|w| w[0] <= w[1]), "{starts:?}");
        for c in &sc.method_calls {
            assert_eq!(sc.method_call_at(c.span).map(|f| f.span), Some(c.span));
        }
    }
    for c in tree.calls() {
        assert_eq!(tree.call_at(c.span).map(|f| f.span), Some(c.span));
    }
}

#[test]
fn print_lowers_like_echo() {
    let src = "<?php\nfunction f($a) { print g($a); }\n";
    let tree = SourceTree::parse(src);
    let f = tree
        .scopes()
        .iter()
        .find(|sc| matches!(&sc.owner, steins_syntax::ScopeOwner::Function { name: n, .. } if n == "f"))
        .expect("the function's scope");
    assert!(matches!(&f.stmts[0].kind, steins_syntax::StmtKind::Echo(calls) if calls.len() == 1));
}
