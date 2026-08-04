//! PHP docblocks attach only to an immediately following declaration or
//! statement when the gap is whitespace (ADR-0029, ADR-0073). Attributes belong
//! to the declaration, and attached spans preserve the exact source slice.

use steins_syntax::SourceTree;

#[test]
fn function_adopts_immediately_preceding_docblock() {
    let src = "<?php\n/** @param int $n */\nfunction f($n): void {}\n";
    let tree = SourceTree::parse(src);
    let doc = tree.functions()[0].docblock.as_deref().expect("docblock attached");
    assert!(doc.contains("@param int $n"));
}

#[test]
fn method_adopts_immediately_preceding_docblock() {
    let src = "<?php\nclass C {\n  /** @return string */\n  public function m(): string { return \"\"; }\n}\n";
    let tree = SourceTree::parse(src);
    let m = &tree.classes()[0].methods[0];
    assert!(m.docblock.as_deref().unwrap().contains("@return string"));
}

#[test]
fn docblock_before_attributes_still_attaches() {
    let src = "<?php\n/** @param int $n */\n#[SomeAttr]\nfunction f($n): void {}\n";
    let tree = SourceTree::parse(src);
    assert!(tree.functions()[0].docblock.as_deref().unwrap().contains("@param int $n"));
}

#[test]
fn floating_docblock_separated_by_code_does_not_attach() {
    let src = "<?php\n/** @param int $n */\n$x = 1;\nfunction f($n): void {}\n";
    let tree = SourceTree::parse(src);
    assert!(tree.functions()[0].docblock.is_none(), "intervening code breaks association");
}

#[test]
fn undocumented_function_has_no_docblock() {
    let src = "<?php\nfunction f($n): void {}\n";
    let tree = SourceTree::parse(src);
    assert!(tree.functions()[0].docblock.is_none());
}

#[test]
fn docblock_span_maps_text_back_to_the_file() {
    // Transform offsets rely on `docblock_span` selecting the exact source (ADR-0034).
    let src = "<?php\n/** @param int $n */\nfunction f($n): void {}\n";
    let tree = SourceTree::parse(src);
    let f = &tree.functions()[0];
    let span = f.docblock_span.expect("span present when docblock attached");
    let from_file = &src[span.start as usize..span.end as usize];
    assert_eq!(from_file, f.docblock.as_deref().unwrap());
    assert_eq!(from_file, "/** @param int $n */");
}

#[test]
fn no_docblock_means_no_span() {
    let src = "<?php\nfunction f($n): void {}\n";
    let tree = SourceTree::parse(src);
    assert!(tree.functions()[0].docblock_span.is_none());
}

use steins_syntax::ScopeOwner;

fn fn_scope(tree: &SourceTree) -> &steins_syntax::Scope {
    tree.scopes()
        .iter()
        .find(|s| matches!(s.owner, ScopeOwner::Function(_)))
        .expect("function scope")
}

#[test]
fn statement_adopts_immediately_preceding_docblock() {
    let src = "<?php\nfunction f(array $a): void {\n  /** @var array{a: int} $a */\n  foo($a);\n}\n";
    let tree = SourceTree::parse(src);
    let start = fn_scope(&tree).stmts[0].span.start;
    let doc = tree.stmt_docblock(start).expect("adjacent docblock");
    assert!(doc.text.contains("@var array{a: int} $a"));
}

#[test]
fn a_comment_in_the_gap_breaks_statement_adjacency() {
    let src = "<?php\nfunction f(array $a): void {\n  /** @var array{a: int} $a */\n  // note\n  foo($a);\n}\n";
    let tree = SourceTree::parse(src);
    let start = fn_scope(&tree).stmts[0].span.start;
    assert!(tree.stmt_docblock(start).is_none(), "a non-doc comment owns the gap");
}

#[test]
fn a_statement_in_the_gap_breaks_statement_adjacency() {
    let src = "<?php\nfunction f(array $a): void {\n  /** @var array{a: int} $a */\n  $x = 1;\n  foo($a);\n}\n";
    let tree = SourceTree::parse(src);
    let scope = fn_scope(&tree);
    assert!(tree.stmt_docblock(scope.stmts[0].span.start).is_some());
    assert!(tree.stmt_docblock(scope.stmts[1].span.start).is_none());
}

#[test]
fn a_property_docblock_never_reaches_a_method_statement() {
    let src = "<?php\nclass C {\n  /** @var int $x */\n  public $x;\n  public function m(): void { foo(1); }\n}\n";
    let tree = SourceTree::parse(src);
    let scope = tree
        .scopes()
        .iter()
        .find(|s| matches!(&s.owner, ScopeOwner::Method { .. }))
        .expect("method scope");
    assert!(tree.stmt_docblock(scope.stmts[0].span.start).is_none());
}
