//! What a class-like body says by name about a destructor and the traits it imports
//! (ADR-0100 §7, issue #882): the lowering the destructor gate's shard tables are
//! built from. A trait's methods are not lowered, so these read the member list.

use steins_syntax::{AnonClassEdge, ClassDecl, NameRef, SourceTree};

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

fn decl(source: &str, name: &str) -> ClassDecl {
    let tree = SourceTree::parse(source);
    tree.classes().iter().find(|c| c.name == name).expect("declaration present").clone()
}

#[test]
fn a_class_and_a_trait_report_the_traits_they_import_and_their_own_destructor() {
    let src = "<?php\n\
        trait Quiet { public function hello() {} }\n\
        trait Loud { public function __Destruct() {} }\n\
        trait Aliasing { use Quiet { hello as __destruct; } }\n\
        trait Nesting { use Quiet, Loud; }\n\
        class Plain { use Quiet; }\n\
        class Holder { use Quiet { hello as protected __destruct; } public function m() {} }\n\
        interface Closer { public function __destruct(); }\n\
        class Free {}\n";
    let quiet = decl(src, "Quiet");
    assert!(quiet.is_trait && !quiet.declares_destructor && quiet.used_traits.is_empty());
    assert!(!quiet.uses_traits, "the obstacle bit stays off for a trait");
    assert!(decl(src, "Loud").declares_destructor, "a trait's methods are read by name");
    let aliasing = decl(src, "Aliasing");
    assert!(aliasing.declares_destructor, "an alias at the trait level");
    assert_eq!(names(&aliasing.used_traits), ["Quiet"]);
    assert_eq!(names(&decl(src, "Nesting").used_traits), ["Quiet", "Loud"]);
    let plain = decl(src, "Plain");
    assert!(plain.uses_traits && !plain.declares_destructor);
    assert_eq!(names(&plain.used_traits), ["Quiet"]);
    assert!(decl(src, "Holder").declares_destructor, "an alias at the class level");
    assert!(decl(src, "Closer").declares_destructor, "an interface's abstract one");
    let free = decl(src, "Free");
    assert!(!free.declares_destructor && free.used_traits.is_empty());
}

#[test]
fn a_trait_reports_the_classes_its_properties_are_hinted_with() {
    let held = |body: &str| {
        let decl = decl(&format!("<?php\ntrait T {{ {body} }}\n"), "T");
        decl.held_classes.iter().map(|r| r.raw.clone()).collect::<Vec<_>>()
    };
    assert_eq!(held("private ?D $d = null;"), ["D"]);
    assert_eq!(held("public D|int|\\Vendor\\E $d = 1;"), ["D", "Vendor\\E"]);
    assert_eq!(held("public function __construct(private D $d, int $n, E $plain) {}"), ["D"]);
    assert_eq!(held("private A $a; protected ?B $b = null; public C&F $c;"), ["A", "B", "C", "F"]);
    assert!(held("public self $s; public parent $p;").is_empty(), "self and parent are asked anyway");
    assert!(held("private int $n = 0; private string $s = ''; private ?array $a = null;").is_empty());
    assert!(held("private static ?D $shared = null;").is_empty());
    assert!(held("public function m(D $d) {}").is_empty());
    // Only a trait reports them: a class's own properties are lowered.
    assert!(decl("<?php\nclass C { private D $d; }\n", "C").held_classes.is_empty());
}

#[test]
fn a_trait_property_hinted_self_or_parent_is_flagged_and_an_anonymous_class_reports_its_held_classes() {
    let flags = |body: &str| {
        let d = decl(&format!("<?php\ntrait T {{ {body} }}\n"), "T");
        (d.holds_self, d.holds_parent)
    };
    assert_eq!(flags("public ?self $a = null;"), (true, false));
    assert_eq!(flags("public ?parent $a = null;"), (false, true));
    assert_eq!(flags("public self|parent|null $a = null; public static ?self $s = null;"), (true, true));
    assert_eq!(flags("public ?D $a = null; public function m(self $x) {}"), (false, false));
    let edges = anon(
        "<?php\n$a = new class extends P { public ?D $d = null; \
         public function __construct(private E $e, int $n) {} public static ?F $f = null; };\n",
    );
    assert_eq!(names(&edges[0].held_classes), ["D", "E"]);
}
