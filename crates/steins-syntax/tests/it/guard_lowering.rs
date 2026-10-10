//! What the lowering records for guard discharge (issue #928): the right-operand
//! extent of `&&`/`||`, the per-statement guard regions, the `switch (true)` chain,
//! and the `dl()` dam site.
//!
//! What the walk does with each is measured in steins-infer's `guard_positions`; these
//! pin the lowering itself, so a construct that loses its extent, falls back to
//! `Opaque`, or stops being recorded fails here under its own name.

use steins_syntax::{
    CondExpr, DynamismKind, GuardRegion, ScopeOwner, SourceTree, Span, Stmt, StmtKind,
};

/// The top-level statements of `function f({params}): void { {body} }`, and the source.
fn lowered(params: &str, body: &str) -> (Vec<Stmt>, String) {
    let src = format!("<?php\nfunction f({params}): void {{\n{body}\n}}\n");
    let tree = SourceTree::parse(&src);
    let f = tree
        .scopes()
        .iter()
        .find(|sc| matches!(&sc.owner, ScopeOwner::Function { name, .. } if name == "f"));
    (f.expect("the function's scope").stmts.clone(), src)
}

fn text(src: &str, span: Span) -> &str {
    &src[span.start as usize..span.end as usize]
}

#[test]
fn an_and_condition_carries_its_right_operands_extent() {
    let (got, src) = lowered("", "if (defined('X') && X > 1) {}");
    let [Stmt { kind: StmtKind::If { cond: CondExpr::And(_, _, span), .. }, .. }] = &got[..] else {
        panic!("{got:?}");
    };
    assert_eq!(text(&src, span.0), "X > 1");
}

#[test]
fn an_or_condition_carries_its_right_operands_extent() {
    let (got, src) = lowered("", "if (!defined('X') or (X > 1)) {}");
    let [Stmt { kind: StmtKind::If { cond: CondExpr::Or(_, _, span), .. }, .. }] = &got[..] else {
        panic!("{got:?}");
    };
    // The extent is the operand as the parser sees it, parentheses included.
    assert_eq!(text(&src, span.0), "(X > 1)");
}

#[test]
fn a_synthesized_connective_has_no_extent() {
    // A multi-argument `isset` is a conjunction the lowering invents: its operands'
    // own extents are real, the chain has no more.
    let (got, src) = lowered("array $a", "if (isset($a['x'], $a['y'])) {}");
    let [Stmt { kind: StmtKind::If { cond: CondExpr::And(_, _, span), .. }, .. }] = &got[..] else {
        panic!("{got:?}");
    };
    assert_eq!(text(&src, span.0), "$a['y']");
    // `empty($a['k'])` desugars to `!isset || !…`, which has no source operand to point at.
    let (got, _) = lowered("array $a", "if (empty($a['k'])) {}");
    let [Stmt { kind: StmtKind::If { cond: CondExpr::Or(_, _, span), .. }, .. }] = &got[..] else {
        panic!("{got:?}");
    };
    assert!(span.is_empty(), "{span:?}");
}

fn guards_of(body: &str) -> (Vec<GuardRegion>, String) {
    let (stmts, src) = lowered("", body);
    let [stmt] = &stmts[..] else { panic!("{stmts:?}") };
    (stmt.guards.clone(), src)
}

#[test]
fn a_ternary_in_every_expression_position_is_a_region_with_both_arms() {
    for body in [
        "return defined('X') ? X : null;",
        "echo defined('X') ? X : '';",
        "take(defined('X') ? X : null);",
        "$v = defined('X') ? X : null;",
        "defined('X') ? X : null;",
        "echo 'a' . (defined('X') ? X : '');",
    ] {
        let (regions, src) = guards_of(body);
        let [region] = &regions[..] else { panic!("`{body}`: {regions:?}") };
        let then = region.dead_if_false.unwrap_or_else(|| panic!("`{body}`: no then arm"));
        let other = region.dead_if_true.unwrap_or_else(|| panic!("`{body}`: no else arm"));
        assert_eq!(text(&src, then), "X", "`{body}`");
        assert!(["null", "''"].contains(&text(&src, other)), "`{body}`: {}", text(&src, other));
        assert!(matches!(region.cond, CondExpr::Call { .. }), "`{body}`: {:?}", region.cond);
    }
}

#[test]
fn a_short_ternary_kills_only_its_else_arm_when_the_test_is_true() {
    let (regions, src) = guards_of("return defined('X') ?: X;");
    let [region] = &regions[..] else { panic!("{regions:?}") };
    assert_eq!(text(&src, region.dead_if_true.expect("else arm")), "X");
    assert!(region.dead_if_false.is_none(), "{region:?}");
}

#[test]
fn a_logical_expression_is_one_region_for_the_whole_chain() {
    // `a && b && c` is one region, not two: evaluating its condition reaches every
    // connective in it, and a second region would lower the same prefix again.
    let (regions, _) = guards_of("defined('A') && defined('B') && defined('C');");
    let [region] = &regions[..] else { panic!("{regions:?}") };
    assert!(matches!(region.cond, CondExpr::And(..)), "{:?}", region.cond);
    assert!(region.dead_if_true.is_none() && region.dead_if_false.is_none());
    // A connective inside a call argument is a region of its own.
    let (regions, _) = guards_of("take(defined('A') && defined('B'));");
    assert_eq!(regions.len(), 1, "{regions:?}");
}

#[test]
fn a_test_that_could_not_decide_without_an_environment_is_not_carried() {
    // Variables are not in the sweep's (empty) environment, so these could only ever be
    // `Maybe` there; carrying them would only grow the trace.
    for body in [
        "return $x ? 1 : 2;",
        "$x && take(1);",
        "return $x === 1 ? 1 : 2;",
        "return isset($x) ? 1 : 2;",
    ] {
        let (stmts, _) = lowered("mixed $x", body);
        let [stmt] = &stmts[..] else { panic!("{stmts:?}") };
        assert!(stmt.guards.is_empty(), "`{body}`: {:?}", stmt.guards);
    }
}

#[test]
fn a_closure_body_is_its_own_scope_and_adds_no_region() {
    let (stmts, _) = lowered("", "$f = function () { return defined('X') ? X : 1; };");
    let [stmt] = &stmts[..] else { panic!("{stmts:?}") };
    assert!(stmt.guards.is_empty(), "{:?}", stmt.guards);
}

#[test]
fn switch_true_with_call_cases_lowers_to_an_if_chain() {
    let (got, src) = lowered(
        "",
        "switch (true) { case defined('A'): return A; case defined('B'): case defined('C'): \
         return B; default: return null; }",
    );
    let [Stmt { kind: StmtKind::If { cond, then_trace, elseifs, else_trace }, .. }] = &got[..]
    else {
        panic!("{got:?}");
    };
    assert!(matches!(cond, CondExpr::Call { .. }), "{cond:?}");
    assert_eq!(then_trace.len(), 1);
    let [(second, _)] = &elseifs[..] else { panic!("{elseifs:?}") };
    // Stacked empty labels join with `||`, the later label's extent on the connective.
    let CondExpr::Or(_, _, span) = second else { panic!("{second:?}") };
    assert_eq!(text(&src, span.0), "defined('C')");
    assert_eq!(else_trace.as_ref().map(Vec::len), Some(1));
}

#[test]
fn switch_true_ends_without_a_break_only_in_its_last_case() {
    let (got, _) = lowered("", "switch (true) { case defined('A'): take(1); }");
    assert!(matches!(&got[..], [Stmt { kind: StmtKind::If { .. }, .. }]), "{got:?}");
    // A middle case that runs on would fall into the next one: not modelled.
    let (got, _) = lowered(
        "",
        "switch (true) { case defined('A'): take(1); case defined('B'): take(2); break; }",
    );
    assert!(matches!(&got[..], [Stmt { kind: StmtKind::Opaque { .. }, .. }]), "{got:?}");
}

#[test]
fn switch_true_reads_trailing_empty_labels_as_no_ops() {
    let (got, _) = lowered("", "switch (true) { case defined('A'): take(1); break; default: }");
    let [Stmt { kind: StmtKind::If { else_trace, .. }, .. }] = &got[..] else { panic!("{got:?}") };
    assert_eq!(else_trace.as_ref().map(Vec::len), Some(0));
}

#[test]
fn a_default_sharing_a_body_before_a_later_case_is_opaque() {
    // `case A: default:` and `default: case A:` share one body; a later case could match
    // the shared label first, so the shared body is not the `else`: not modelled.
    for body in [
        "switch (true) { case defined('A'): default: take(1); break; case defined('B'): take(2); }",
        "switch (true) { default: case defined('A'): take(1); break; case defined('B'): take(2); }",
    ] {
        let (got, _) = lowered("", body);
        let opaque = matches!(&got[..], [Stmt { kind: StmtKind::Opaque { .. }, .. }]);
        assert!(opaque, "`{body}`: {got:?}");
    }
    for body in [
        "switch ($x) { case 1: default: take(1); break; case 2: take(2); break; }",
        "switch ($x) { default: case 1: take(1); break; case 2: take(2); break; }",
    ] {
        let (got, _) = lowered("int $x", body);
        let opaque = matches!(&got[..], [Stmt { kind: StmtKind::Opaque { .. }, .. }]);
        assert!(opaque, "`{body}`: {got:?}");
    }
}

#[test]
fn a_default_sharing_the_last_body_is_the_else() {
    for body in [
        "switch (true) { case defined('B'): take(2); break; case defined('A'): default: take(1); }",
        "switch (true) { case defined('B'): take(2); break; default: case defined('A'): take(1); }",
    ] {
        let (got, _) = lowered("", body);
        let [Stmt { kind: StmtKind::If { elseifs, else_trace, .. }, .. }] = &got[..] else {
            panic!("`{body}`: {got:?}");
        };
        assert!(elseifs.is_empty(), "`{body}`: {elseifs:?}");
        assert_eq!(else_trace.as_ref().map(Vec::len), Some(1), "`{body}`");
    }
    let (got, _) = lowered(
        "int $x",
        "switch ($x) { case 2: take(2); break; case 1: default: take(1); break; }",
    );
    let by_value =
        matches!(&got[..], [Stmt { kind: StmtKind::Match { default: Some(_), .. }, .. }]);
    assert!(by_value, "{got:?}");
}

#[test]
fn a_by_value_switch_takes_the_same_last_case_rule() {
    // ADR-0103 D4 extends the chain's relaxation to `switch ($x)`: a last case that
    // runs off its end leaves the switch, so the construct is structured.
    let (got, _) = lowered("int $x", "switch ($x) { case 1: take(1); }");
    let by_value = matches!(&got[..], [Stmt { kind: StmtKind::Match { loose: true, .. }, .. }]);
    assert!(by_value, "{got:?}");
    let (got, _) =
        lowered("int $x", "switch ($x) { case 1: take(1); break; default: take(2); break; }");
    let by_value = matches!(&got[..], [Stmt { kind: StmtKind::Match { loose: true, .. }, .. }]);
    assert!(by_value, "{got:?}");
}

#[test]
fn a_chain_with_a_landing_jump_stays_opaque() {
    // An `if` has no landing edge to carry (ADR-0103), so a `switch (true)` case whose
    // jump lands on the successor keeps the construct `Opaque`; a jump that only
    // ends the arm does not.
    let (got, _) = lowered(
        "",
        "switch (true) { case defined('A'): foreach ([1] as $v) { break 2; } return; default: take(1); }",
    );
    assert!(matches!(&got[..], [Stmt { kind: StmtKind::Opaque { .. }, .. }]), "{got:?}");
    let (got, _) = lowered(
        "",
        "while (true) { switch (true) { case defined('A'): break 2; default: take(1); } }",
    );
    let [Stmt { kind: StmtKind::While { body, .. }, .. }] = &got[..] else { panic!("{got:?}") };
    assert!(matches!(&body[..], [Stmt { kind: StmtKind::If { .. }, .. }]), "{body:?}");
}

#[test]
fn a_dl_call_is_a_dynamism_site() {
    let src = "<?php\nfunction f() { dl('redis.so'); \\dl('x'); Ns\\dl('y'); \\Ns\\dl('z'); }\n";
    let tree = SourceTree::parse(src);
    let kinds: Vec<_> = tree.dynamism_sites().iter().map(|s| s.kind.clone()).collect();
    assert_eq!(kinds, [DynamismKind::ExtensionLoad, DynamismKind::ExtensionLoad], "{kinds:?}");
}
