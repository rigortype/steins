//! Grouped `use A\{B, C}` lowering (ADR-0049 §6 follow-up: a resolution bug fix).
//!
//! Grouped-use imports were previously skipped, so a class named by a grouped
//! import fell back through namespace resolution and could **mis-resolve** to an
//! unrelated class of the fallback name (a false-positive source the arity check
//! surfaced). These tests lock the lowering: the grouped forms populate the same
//! `class_imports` / `fn_imports` maps the plain sequence forms do.

use steins_syntax::SourceTree;

/// The namespace context covering a byte offset just inside `needle`.
fn imports_at<'a>(tree: &'a SourceTree, src: &str, needle: &str) -> &'a steins_syntax::NsCtx {
    let off = (src.find(needle).expect("needle present") + 1) as u32;
    tree.ctx_at(off)
}

#[test]
fn grouped_class_use_resolves_each_item() {
    let src = "<?php\nuse Contentful\\{Delivery\\Query, Core\\File\\ImageFile};\n$q = new Query();\n";
    let tree = SourceTree::parse(src);
    let ctx = imports_at(&tree, src, "new Query");
    assert_eq!(ctx.class_imports.get("query").map(String::as_str), Some("Contentful\\Delivery\\Query"));
    assert_eq!(
        ctx.class_imports.get("imagefile").map(String::as_str),
        Some("Contentful\\Core\\File\\ImageFile")
    );
}

#[test]
fn grouped_class_use_alias_form() {
    let src = "<?php\nuse App\\{Model\\User as U, Model\\Post};\n$u = new U();\n";
    let tree = SourceTree::parse(src);
    let ctx = imports_at(&tree, src, "new U");
    // The `as U` alias keys the import; the default alias is the last segment.
    assert_eq!(ctx.class_imports.get("u").map(String::as_str), Some("App\\Model\\User"));
    assert_eq!(ctx.class_imports.get("post").map(String::as_str), Some("App\\Model\\Post"));
}

#[test]
fn grouped_function_use() {
    let src = "<?php\nuse function App\\Helpers\\{format, slugify};\nformat();\n";
    let tree = SourceTree::parse(src);
    let ctx = imports_at(&tree, src, "format()");
    assert_eq!(ctx.fn_imports.get("format").map(String::as_str), Some("App\\Helpers\\format"));
    assert_eq!(ctx.fn_imports.get("slugify").map(String::as_str), Some("App\\Helpers\\slugify"));
    // A function group must not pollute the class-import map.
    assert!(!ctx.class_imports.contains_key("format"));
}

#[test]
fn mixed_group_use_splits_by_item_type() {
    let src = "<?php\nuse App\\{Model\\User, function util\\fmt, const util\\MAX};\n$u = new User();\n";
    let tree = SourceTree::parse(src);
    let ctx = imports_at(&tree, src, "new User");
    // Class item → class_imports.
    assert_eq!(ctx.class_imports.get("user").map(String::as_str), Some("App\\Model\\User"));
    // Function item → fn_imports.
    assert_eq!(ctx.fn_imports.get("fmt").map(String::as_str), Some("App\\util\\fmt"));
    // `const` item → skipped (out of scope), never a class/function import.
    assert!(!ctx.class_imports.contains_key("max"));
    assert!(!ctx.fn_imports.contains_key("max"));
}

#[test]
fn plain_sequence_use_still_lowers() {
    // Regression guard: the non-grouped forms are unchanged.
    let src = "<?php\nuse App\\Model\\User, App\\Model\\Post as P;\nuse function App\\fmt;\n$u = new User();\n";
    let tree = SourceTree::parse(src);
    let ctx = imports_at(&tree, src, "new User");
    assert_eq!(ctx.class_imports.get("user").map(String::as_str), Some("App\\Model\\User"));
    assert_eq!(ctx.class_imports.get("p").map(String::as_str), Some("App\\Model\\Post"));
    assert_eq!(ctx.fn_imports.get("fmt").map(String::as_str), Some("App\\fmt"));
}
