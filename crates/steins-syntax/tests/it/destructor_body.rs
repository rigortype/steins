//! What a class-like body says by name about a destructor and the traits it imports
//! (ADR-0100 §7, issue #882): the lowering the destructor gate's shard tables are
//! built from. A trait's methods are not lowered, so these read the member list.

use steins_syntax::{AnonClassEdge, NameRef, SourceTree};

fn anon(source: &str) -> Vec<AnonClassEdge> {
    SourceTree::parse(source).anonymous_class_edges().to_vec()
}

fn names(refs: &[NameRef]) -> Vec<&str> {
    refs.iter().map(|r| r.raw.as_str()).collect()
}

#[test]
fn an_anonymous_class_reports_its_own_destructor_and_imported_traits() {
    let edges = anon(
        "<?php\n\
         $a = new class extends P {};\n\
         $b = new class extends P { public function __destruct() {} };\n\
         $c = new class extends P { use T, U; };\n\
         $d = new class extends P { use T { bye as __destruct; } };\n\
         $e = new class extends P { use T { bye as protected hello; } };\n\
         $f = new class extends P { public function __DESTRUCT() {} };\n",
    );
    assert_eq!(edges.len(), 6);
    let own = |i: usize| (edges[i].declares_destructor, names(&edges[i].used_traits));
    assert_eq!(own(0), (false, vec![]), "a clean body");
    assert_eq!(own(1), (true, vec![]), "a declared destructor");
    assert_eq!(own(2), (false, vec!["T", "U"]), "imports are listed, not read");
    assert_eq!(own(3), (true, vec!["T"]), "an alias named __destruct is the destructor");
    assert_eq!(own(4), (false, vec!["T"]), "any other alias is not");
    assert_eq!(own(5), (true, vec![]), "method names are case-insensitive");
}

#[test]
fn a_destructor_in_a_nested_class_belongs_to_that_class() {
    let edges = anon(
        "<?php\n$a = new class extends P { public function m() { \
         return new class extends Q { public function __destruct() {} }; } };\n",
    );
    assert_eq!(edges.len(), 2);
    let outer = edges.iter().find(|e| e.parent.as_ref().is_some_and(|p| p.raw == "P")).unwrap();
    let inner = edges.iter().find(|e| e.parent.as_ref().is_some_and(|p| p.raw == "Q")).unwrap();
    assert!(!outer.declares_destructor, "the nested class's method is not the outer's");
    assert!(inner.declares_destructor);
}
