//! Tests for docblock association on declarations (ADR-0029): the `/** … */`
//! trivium immediately preceding a function/method (only whitespace between,
//! attributes included on the declaration side) is attached; a floating docblock
//! separated by intervening code is not.

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
    // The declaration span starts at the attribute list, so a docblock above the
    // attribute is still separated only by whitespace.
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
