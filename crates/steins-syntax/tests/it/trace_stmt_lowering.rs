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
    let f = tree.scopes().iter().find(|sc| matches!(&sc.owner, ScopeOwner::Function { name: n, .. } if n == "f"));
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
fn a_do_while_records_whether_no_jump_in_it_comes_back_to_the_loop() {
    // Issue #679, relaxed by #904 step 3: the gate is `break_free && continue_free`,
    // read off the CST body with jumps credited by level (ADR-0103). A jump aimed at
    // a nested loop or `switch` is that construct's, however many levels it climbs
    // inside the body; one aimed at this loop, an outer one, or a `goto` is refused.
    let gate = |body: &str| {
        let got = stmts("int $n", &format!("do {{ {body} }} while ($n > 0);"));
        match &got[..] {
            [Stmt { kind: StmtKind::DoWhile { break_free, continue_free, .. }, .. }] => {
                *break_free && *continue_free
            }
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
        // Multi-level jumps that stay inside the body.
        "while ($n) { while ($n) { break 2; } }",
        "while ($n) { while ($n) { continue 2; } }",
        "while ($n) { switch ($n) { case 1: continue 2; } }",
        "switch ($n) { case 1: switch ($n) { case 2: break 2; } }",
        // A jump a terminating `finally` replaces lands nowhere.
        "try { break; } finally { return; }",
        "try { continue; } finally { throw new Exception(); }",
    ] {
        assert!(gate(body), "no jump in `{body}` comes back to the loop");
    }
    for body in [
        "continue;",
        "break;",
        "if ($n) { continue; }",
        "while ($n) { continue 2; }",
        "while ($n) { break 2; }",
        "switch ($n) { case 1: continue 2; }",
        "switch ($n) { case 1: break 2; }",
        "foreach ([1] as $v) { switch ($v) { case 1: continue 3; } }",
        "while ($n) { while ($n) { break 3; } }",
        "while ($n) { while ($n) { continue 4; } }",
        "while ($n) { break $n; }",
        "goto done; done: return;",
        "try { break; } finally { echo 1; }",
    ] {
        assert!(!gate(body), "`{body}` holds a jump that comes back or leaves");
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

/// One arm's landing writes, `None` for an arm whose jumps do not land on the
/// successor.
type Landing = Option<Vec<String>>;

/// The arms of the one structured `switch` in `body`: each arm's [`Landing`], then
/// the `default` body's.
fn switch_landings(body: &str) -> (Vec<Landing>, Option<Landing>) {
    let got = stmts("int $n, bool $c", body);
    match &got[..] {
        [Stmt { kind: StmtKind::Match { arms, default, default_lands, loose: true, .. }, .. }, ..] => {
            let arms = arms.iter().map(|a| a.lands.as_ref().map(|l| l.writes.clone())).collect();
            let default = default.as_ref().map(|_| default_lands.as_ref().map(|l| l.writes.clone()));
            (arms, default)
        }
        other => panic!("the `switch` lowered to {other:?}"),
    }
}

#[test]
fn a_switch_arm_carries_the_jumps_that_land_on_its_successor() {
    // ADR-0103: a `break N`/`continue N` written N - 1 breakable constructs deep lands
    // on the successor, and so does any `goto`; a deeper jump ends the arm.
    let lands = |writes: &[&str]| Some(writes.iter().map(|w| (*w).to_owned()).collect());
    let (arms, default) = switch_landings(
        "switch ($n) { case 1: $a = 1; foreach ([1] as $v) { break 2; } return; \
         case 2: foreach ([1] as $v) { break 3; } return; \
         case 3: if ($c) { continue; } return; \
         case 4: while ($c) { continue 2; } return; \
         case 5: goto out; \
         default: switch ($c) { case true: break 2; } return; } out: return;",
    );
    assert_eq!(arms, vec![lands(&["a", "v"]), None, lands(&[]), lands(&[]), lands(&[])]);
    assert_eq!(default, Some(lands(&[])));
}

#[test]
fn a_last_case_may_end_and_trailing_labels_are_empty_arms() {
    // ADR-0103 D4: the last non-empty case runs off its end; the empty labels after
    // it become an empty arm and an empty `default`.
    let body = "switch ($n) { case 1: echo 1; break; case 2: echo 2; case 3: default: }";
    match &stmts("int $n", body)[..] {
        [Stmt { kind: StmtKind::Match { arms, default: Some(default), .. }, .. }] => {
            assert_eq!(arms.len(), 2, "{arms:?}");
            assert!(default.is_empty(), "{default:?}");
        }
        other => panic!("the `switch` lowered to {other:?}"),
    }
    match &stmts("int $n", "switch ($n) { case 1: echo 1; break; case 2: case 3: }")[..] {
        [Stmt { kind: StmtKind::Match { arms, default: None, .. }, .. }] => {
            assert_eq!(arms.len(), 2, "{arms:?}");
            assert_eq!(arms[1].conditions.len(), 2, "{arms:?}");
            assert!(arms[1].trace.is_empty(), "{arms:?}");
        }
        other => panic!("the `switch` lowered to {other:?}"),
    }
    // A case that runs into a non-empty one stays unstructured.
    let got = stmts("int $n", "switch ($n) { case 1: echo 1; case 2: echo 2; }");
    assert!(matches!(&got[..], [Stmt { kind: StmtKind::Opaque { .. }, .. }]), "{got:?}");
}

#[test]
fn a_jump_a_terminating_finally_replaces_lands_nowhere() {
    // ADR-0103: `try { break; } finally { return; }` returns (PHP 8.5.11), so the
    // `break` neither lands on a switch's successor nor leaves a loop.
    let (arms, _) = switch_landings(
        "switch ($n) { case 1: try { break; } finally { return; } \
         case 2: try { break; } finally { echo 1; } return; }",
    );
    assert_eq!(arms, vec![None, Some(Vec::new())]);
    let got = stmts("int $n", "while ($n > 0) { try { break; } finally { return; } }");
    match &got[..] {
        [Stmt { kind: StmtKind::While { break_free, .. }, .. }] => assert!(*break_free),
        other => panic!("the `while` lowered to {other:?}"),
    }
    let got = stmts("int $n", "while ($n > 0) { try { break; } finally { echo 1; } }");
    match &got[..] {
        [Stmt { kind: StmtKind::While { break_free, .. }, .. }] => assert!(!*break_free),
        other => panic!("the `while` lowered to {other:?}"),
    }
}

#[test]
fn a_goto_landing_carries_no_env() {
    // Only a `break`/`continue` landing sets `by_level`; a `goto` keeps the
    // successor reachable and nothing more.
    let by_level = |body: &str| match &stmts("int $n, bool $c", body)[..] {
        [Stmt { kind: StmtKind::Match { arms, .. }, .. }, ..] => {
            arms.iter().map(|a| a.lands.as_ref().map(|l| l.by_level)).collect::<Vec<_>>()
        }
        other => panic!("the `switch` lowered to {other:?}"),
    };
    assert_eq!(
        by_level("switch ($n) { case 1: goto out; case 2: if ($c) { goto out; } break; } out: return;"),
        vec![Some(false), Some(false)]
    );
    assert_eq!(
        by_level("switch ($n) { case 1: if ($c) { goto out; } if ($c) { break; } return; } out: return;"),
        vec![Some(true)]
    );
}

#[test]
fn a_goto_ends_its_list_and_a_label_marks_where_a_walk_resumes() {
    // ADR-0103: a `goto` is a terminator; a label, and an `if` or `try` holding one,
    // say where a walk resumes. A label in a loop does not count outside it.
    let got = stmts(
        "bool $c",
        "goto out; $x = 1; out: echo 1; if ($c) { in: echo 2; } try { t: echo 3; } finally {} \
         while ($c) { w: break; }",
    );
    let kinds: Vec<(&str, bool)> = got
        .iter()
        .map(|s| {
            let k = match &s.kind {
                StmtKind::LoopJump { .. } => "jump",
                StmtKind::Label { .. } => "label",
                StmtKind::If { .. } => "if",
                StmtKind::Try { .. } => "try",
                StmtKind::While { .. } => "while",
                _ => "other",
            };
            (k, s.holds_label)
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("jump", false),
            ("other", false),
            ("label", true),
            ("other", false),
            ("if", true),
            ("try", true),
            ("while", false),
        ]
    );
}

// ---- issue #1048: what a value the lowering cannot spell may hand on -----------

#[test]
fn an_unspelled_rvalue_names_what_it_mentions() {
    // A `match` and an `(object)` cast lower to `Other`; the names inside them
    // are kept, so the walk can escape the objects they may store.
    let got = stmts("", "$q = match ($k) { 0 => $o, default => $p }; $w = (object) ['o' => $r];");
    let reads: Vec<Vec<String>> = got
        .iter()
        .filter(|s| matches!(s.kind, StmtKind::Assign { .. }))
        .map(|s| {
            let mut r = s.unspelled_reads.clone();
            r.sort();
            r
        })
        .collect();
    assert_eq!(reads, [vec!["k", "o", "p", "q"], vec!["r", "w"]], "{got:?}");
}

#[test]
fn a_spelled_rvalue_names_nothing() {
    let got = stmts("", "$q = [$o, $k ? $p : 1];");
    assert!(got.iter().all(|s| s.unspelled_reads.is_empty()), "{got:?}");
}

#[test]
fn a_statement_that_yields_is_a_barrier() {
    // The generator's consumer runs before the expression resumes.
    let got = stmts("", "$r = yield $o;");
    assert!(matches!(&got[..], [Stmt { kind: StmtKind::Barrier, .. }]), "{got:?}");
}
