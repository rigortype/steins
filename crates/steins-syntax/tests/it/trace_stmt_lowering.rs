//! The statement shapes the trace IR carries for offset writes (issue #636) and
//! the four structured loops (issues #649, #650, #652).
//!
//! What a walker does with each shape is measured in steins-infer's tests; these
//! pin the lowering itself — which variant a construct becomes and which of its
//! parts the variant keeps — so a construct that falls back to `Opaque` or
//! `Barrier`, or loses a clause, fails here under its own name.

use steins_syntax::{ScopeOwner, SourceTree, Stmt, StmtKind};

/// The top-level statements of `function f({params}): void { {body} }`.
fn stmts(params: &str, body: &str) -> Vec<Stmt> {
    let src = format!("<?php\nfunction f({params}): void {{\n{body}\n}}\n");
    let tree = SourceTree::parse(&src);
    let f = tree.scopes().iter().find(|sc| matches!(&sc.owner, ScopeOwner::Function(n) if n == "f"));
    f.expect("the function's scope").stmts.clone()
}

#[test]
fn an_offset_write_append_and_unset_are_three_variants() {
    let got = stmts("", "$a = []; $a[] = 1; $a['k'] = 2; unset($a['k']);");
    let kinds: Vec<String> = got
        .iter()
        .map(|s| match &s.kind {
            StmtKind::Assign { var, .. } => format!("assign {var}"),
            StmtKind::OffsetAppend { base, .. } => format!("append {base}"),
            StmtKind::OffsetWrite { base, .. } => format!("write {base}"),
            StmtKind::OffsetUnset { base, .. } => format!("unset {base}"),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(kinds, ["assign a", "append a", "write a", "unset a"], "{got:?}");
}

#[test]
fn a_while_keeps_its_body() {
    let got = stmts("int $n", "while ($n > 0) { $s = 'abc'; $n--; }");
    match &got[..] {
        [Stmt { kind: StmtKind::While { body, .. }, .. }] => assert_eq!(body.len(), 2, "{body:?}"),
        other => panic!("the `while` lowered to {other:?}"),
    }
}

#[test]
fn a_for_keeps_its_init_body_and_carried_set() {
    // `$s` is written by `init` alone, so it is carried; `$i` is rewritten by the
    // increment, so it is not.
    let got = stmts("int $n", "for ($i = 0, $s = 'abc'; $i < $n; $i++) { $a = 1; $b = 2; }");
    match &got[..] {
        [Stmt { kind: StmtKind::For { init, body, carried, .. }, .. }] => {
            assert_eq!(init.len(), 2, "{init:?}");
            assert_eq!(body.len(), 2, "{body:?}");
            assert_eq!(carried, &["s".to_owned()]);
        }
        other => panic!("the `for` lowered to {other:?}"),
    }
}

#[test]
fn a_do_while_keeps_its_body() {
    let got = stmts("int $n", "do { $d = 4; } while ($n > 0);");
    match &got[..] {
        [Stmt { kind: StmtKind::DoWhile { body, .. }, .. }] => assert_eq!(body.len(), 1, "{body:?}"),
        other => panic!("the `do`-`while` lowered to {other:?}"),
    }
}

#[test]
fn a_do_while_records_whether_every_jump_in_it_is_a_nested_constructs() {
    // Issue #679: `nested_jumps_only` is read off the CST body. Every `break` and
    // `continue` must have level 1 and sit inside a nested loop or `switch`, and no
    // `goto` may appear; nested function-likes are not descended. A multi-level jump
    // is refused even where it stays inside the body (#904).
    let nested_jumps_only = |body: &str| {
        let got = stmts("int $n", &format!("do {{ {body} }} while ($n > 0);"));
        match &got[..] {
            [Stmt { kind: StmtKind::DoWhile { nested_jumps_only, .. }, .. }] => *nested_jumps_only,
            other => panic!("the `do`-`while` lowered to {other:?}"),
        }
    };
    for body in [
        "return;",
        "while ($n) { continue; }",
        "foreach ([1] as $v) { if ($v) { continue; } }",
        "switch ($n) { case 1: continue; }",
        "switch ($n) { case 1: break; }",
        "$f = function () { foreach ([1] as $v) { continue; } };",
    ] {
        assert!(nested_jumps_only(body), "every jump in `{body}` is a nested construct's");
    }
    for body in [
        "continue;",
        "break;",
        "if ($n) { continue; }",
        "while ($n) { continue 2; }",
        "while ($n) { break 2; }",
        "switch ($n) { case 1: continue 2; }",
        "foreach ([1] as $v) { switch ($v) { case 1: continue 3; } }",
        "goto done; done: return;",
    ] {
        assert!(!nested_jumps_only(body), "`{body}` holds a jump this gate refuses");
    }
}

#[test]
fn a_foreach_keeps_its_header_and_body() {
    let got = stmts(
        "array $xs, array $ys",
        "foreach ($xs as $k => $v) { $a = 1; }\n\
         foreach ($ys as &$r) { $b = 2; }\n\
         foreach (g() as [$p, $q]) { $c = 3; }",
    );
    let headers: Vec<String> = got
        .iter()
        .map(|s| match &s.kind {
            StmtKind::Foreach { subject, key_var, value_var, by_ref, body, .. } => format!(
                "{subject:?} {key_var:?} {value_var:?} {by_ref} body={}",
                body.len()
            ),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        headers,
        [
            "Some(\"xs\") Some(\"k\") Some(\"v\") false body=1",
            // The by-ref target's NAME survives; the flag is what refuses the
            // binding, so a reader must see both.
            "Some(\"ys\") None Some(\"r\") true body=1",
            // A non-variable subject and a destructuring target are both `None`:
            // there is nothing for a walker to ask the env about.
            "None None None false body=1",
        ],
    );
}
