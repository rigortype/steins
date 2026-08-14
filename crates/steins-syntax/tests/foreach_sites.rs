//! The `foreach` lowering the loop→`array_map` transform enumerates (ADR-0076).
//!
//! The transform's own tests measure verdicts; these measure the *shape facts*
//! those verdicts are built from — especially that **every** `foreach` is
//! enumerated (nested, or inside closures) and each carries the right
//! preceding sibling and enclosing-scope end.

use steins_syntax::SourceTree;

fn sites(source: &str) -> Vec<steins_syntax::ForeachSite> {
    SourceTree::parse(source).foreach_sites().to_vec()
}

#[test]
fn every_foreach_is_enumerated_including_nested_ones() {
    let src = "<?php\nforeach ($a as $x) {\n    foreach ($x as $y) {\n        echo $y;\n    }\n}\nfunction f(array $z): void {\n    foreach ($z as $w) {\n        echo $w;\n    }\n}\n$c = function (array $q): void {\n    foreach ($q as $r) {\n        echo $r;\n    }\n};\n";
    assert_eq!(sites(src).len(), 4, "a foreach was dropped");
}

#[test]
fn the_binding_shape_is_reported_faithfully() {
    let value = &sites("<?php\nforeach ($a as $x) {}\n")[0];
    assert_eq!(value.subject.as_deref(), Some("a"));
    assert_eq!(value.value_var.as_deref(), Some("x"));
    assert!(!value.key_binding);
    assert!(!value.by_ref_binding);

    let keyed = &sites("<?php\nforeach ($a as $k => $x) {}\n")[0];
    assert!(keyed.key_binding);

    let by_ref = &sites("<?php\nforeach ($a as &$x) {}\n")[0];
    assert!(by_ref.by_ref_binding);
    // The bound name is still readable behind the `&`; the flag, not blindness, is the fact.
    assert_eq!(by_ref.value_var.as_deref(), Some("x"));

    let destructured = &sites("<?php\nforeach ($a as [$p, $q]) {}\n")[0];
    assert_eq!(destructured.value_var, None);

    let computed = &sites("<?php\nforeach (f() as $x) {}\n")[0];
    assert_eq!(computed.subject, None);
}

#[test]
fn a_braced_single_append_is_a_one_statement_body() {
    // The braced form arrives as one `Statement::Block`; unwrapped it's one
    // statement, not one block.
    let site = &sites("<?php\nforeach ($a as $x) {\n    $out[] = $x * 2;\n}\n")[0];
    assert_eq!(site.body.stmt_count, 1);
    let append = site.body.append.as_ref().expect("append not recognized");
    assert_eq!(append.acc, "out");
    assert_eq!(append.value_vars, vec!["x".to_owned()]);
    assert!(!append.value_writes);
    assert!(!append.value_unmodelled);
}

#[test]
fn an_empty_body_is_zero_statements_not_one() {
    assert_eq!(sites("<?php\nforeach ($a as $x) {}\n")[0].body.stmt_count, 0);
    assert_eq!(sites("<?php\nforeach ($a as $x);\n")[0].body.stmt_count, 0);
}

#[test]
fn an_offset_write_is_not_an_append() {
    let site = &sites("<?php\nforeach ($a as $x) {\n    $out[$x] = 1;\n}\n")[0];
    assert!(site.body.append.is_none(), "an indexed write was read as an append");
}

#[test]
fn early_exits_are_seen_through_nesting_but_not_through_a_closure() {
    let nested = &sites("<?php\nforeach ($a as $x) {\n    if ($x) {\n        continue;\n    }\n}\n")[0];
    assert!(nested.body.early_exit);

    // A `return` inside a closure returns from the closure, not from the loop.
    let closure =
        &sites("<?php\nforeach ($a as $x) {\n    $out[] = (function () { return 1; })();\n}\n")[0];
    assert!(!closure.body.early_exit);
}

#[test]
fn the_preceding_sibling_is_reported_with_what_it_assigns() {
    let init = &sites("<?php\n$out = [];\nforeach ($a as $x) {}\n")[0];
    let prev = init.prev_stmt.as_ref().expect("no preceding statement");
    assert_eq!(prev.assign_target.as_deref(), Some("out"));
    assert!(prev.assigns_empty_array);

    let legacy = &sites("<?php\n$out = array();\nforeach ($a as $x) {}\n")[0];
    assert!(legacy.prev_stmt.as_ref().unwrap().assigns_empty_array);

    let nonempty = &sites("<?php\n$out = [1];\nforeach ($a as $x) {}\n")[0];
    let prev = nonempty.prev_stmt.as_ref().unwrap();
    assert_eq!(prev.assign_target.as_deref(), Some("out"));
    assert!(!prev.assigns_empty_array);

    let compound = &sites("<?php\n$out .= 'x';\nforeach ($a as $x) {}\n")[0];
    assert_eq!(compound.prev_stmt.as_ref().unwrap().assign_target, None);

    // A loop that opens its block has no preceding sibling at all.
    let first = &sites("<?php\nfunction f(array $a): void {\n    foreach ($a as $x) {}\n}\n")[0];
    assert!(first.prev_stmt.is_none());
}

#[test]
fn the_scope_end_is_the_enclosing_function_not_the_file() {
    let src = "<?php\nfunction f(array $a): void {\n    foreach ($a as $x) {}\n}\nfunction g(): void {\n    echo 'x';\n}\n";
    let site = &sites(src)[0];
    let scope_end = site.scope_end as usize;
    assert!(scope_end < src.len(), "the scope ran to the end of the file");
    // The remainder of the scope stops before `function g`.
    assert!(!src[site.span.end as usize..scope_end].contains("function g"));

    // At top level the scope is the file.
    let top = &sites("<?php\nforeach ($a as $x) {}\necho 'tail';\n")[0];
    assert_eq!(top.scope_end as usize, "<?php\nforeach ($a as $x) {}\necho 'tail';\n".len());
}

#[test]
fn unmodelled_constructs_in_the_appended_expression_are_flagged() {
    let flagged = [
        "new Foo()",
        "clone $x",
        "`ls`",
        "compact('x')",
        "get_defined_vars()",
        "func_get_args()",
        "func_num_args()",
        "${$x}",
    ];
    for expr in flagged {
        let src = format!("<?php\nforeach ($a as $x) {{\n    $out[] = {expr};\n}}\n");
        let site = &sites(&src)[0];
        let append = site.body.append.as_ref().unwrap_or_else(|| panic!("no append for `{expr}`"));
        assert!(append.value_unmodelled, "`{expr}` was read as modelled");
    }

    let plain = &sites("<?php\nforeach ($a as $x) {\n    $out[] = strlen($x);\n}\n")[0];
    assert!(!plain.body.append.as_ref().unwrap().value_unmodelled);
}

#[test]
fn a_write_inside_the_appended_expression_is_flagged() {
    for expr in ["$i++", "++$i", "$i = $x", "$x--"] {
        let src = format!("<?php\nforeach ($a as $x) {{\n    $out[] = {expr};\n}}\n");
        let append = sites(&src)[0].body.append.clone().unwrap_or_else(|| panic!("no append for `{expr}`"));
        assert!(append.value_writes, "`{expr}` was read as read-only");
    }
    let plain = &sites("<?php\nforeach ($a as $x) {\n    $out[] = $x + 1;\n}\n")[0];
    assert!(!plain.body.append.as_ref().unwrap().value_writes);
}
