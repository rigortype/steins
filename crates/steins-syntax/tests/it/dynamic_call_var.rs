//! `DynamicSite::Call { var }` (ADR-0100 §4.2): a `$f()` the scan cannot name carries the
//! callee's name only when it is a by-value, non-variadic parameter that no statement of
//! the frame writes, the one case a declared contract on the parameter speaks about.

use steins_syntax::{DynamicSite, SiteKind, SourceTree};

/// The `var` of the `DynamicSite::Call` of `function f(<params>) { <body> }`, one entry
/// per such site in site order.
fn vars(params: &str, body: &str) -> Vec<Option<String>> {
    let src = format!("<?php\nfunction f({params}) {{ {body} }}\n");
    let tree = SourceTree::parse(&src);
    assert!(tree.parse_errors().is_empty(), "{body}: {:?}", tree.parse_errors());
    tree.functions()[0]
        .sites
        .iter()
        .filter_map(|s| match &s.kind {
            SiteKind::Dynamic(DynamicSite::Call { var }) => Some(var.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn an_unwritten_by_value_parameter_names_itself() {
    assert_eq!(vars("callable $f", "return $f();"), [Some("f".to_owned())]);
    assert_eq!(vars("$f, $g", "$f(); return $g();"), [Some("f".to_owned()), Some("g".to_owned())]);
}

#[test]
fn a_write_of_any_kind_withdraws_the_name() {
    for body in [
        "$f = $g; return $f();",
        "$f .= 'x'; return $f();",
        "$f++; return $f();",
        "unset($f); return $f();",
        "foreach ($g as $f) {} return $f();",
    ] {
        assert_eq!(vars("callable $f, $g", body), [None], "{body}");
    }
}

#[test]
fn only_a_by_value_non_variadic_parameter_qualifies() {
    assert_eq!(vars("callable &$f", "return $f();"), [None], "by reference");
    assert_eq!(vars("callable ...$f", "return $f();"), [None], "variadic");
    assert_eq!(vars("", "$f = fn() => 1; return $g();"), [None], "a local is not a parameter");
    assert_eq!(vars("callable $f", "global $f; return $f();"), [None], "an aliasing frame");
    assert_eq!(vars("$f", "return ($f['k'])();"), [None], "not a bare variable");
}
