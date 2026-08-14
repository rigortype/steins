//! Global-constant lowering (ADR-0078, issue #198): the declaration records, the
//! fetch references, the `use const` import map, and the computed-`define()` dam
//! site — everything the `constant.undefined` ladder reads out of a lowered file.
//!
//! The case discipline is the subject of half of it, because it is the one place
//! constants differ from every other name in the language: the **namespace prefix**
//! folds case, the **final segment** does not. Measured on PHP 8.5.9 with
//! `namespace App; const LOCAL = 'l';` — `defined('App\LOCAL')` and
//! `defined('app\LOCAL')` are both `true`, `defined('App\local')` is `false`.

use steins_syntax::{DynamismKind, SourceTree, normalize_const_fqn};

fn decls(src: &str) -> Vec<String> {
    SourceTree::parse(src).global_const_decls().iter().map(|d| d.fqn.clone()).collect()
}

fn fetches(src: &str) -> Vec<String> {
    SourceTree::parse(src).const_refs().iter().map(|r| r.raw.clone()).collect()
}

// The key normalizer.

#[test]
fn normalization_folds_the_namespace_and_keeps_the_final_segment() {
    assert_eq!(normalize_const_fqn("FOO"), "FOO");
    assert_eq!(normalize_const_fqn("\\FOO"), "FOO");
    assert_eq!(normalize_const_fqn("App\\LOCAL"), "app\\LOCAL");
    assert_eq!(normalize_const_fqn("APP\\Sub\\Local"), "app\\sub\\Local");
    // Namespace-folded spellings collapse to one name; final-segment case does not.
    assert_eq!(normalize_const_fqn("App\\LOCAL"), normalize_const_fqn("app\\LOCAL"));
    assert_ne!(normalize_const_fqn("App\\LOCAL"), normalize_const_fqn("App\\local"));
}

// Declarations.

#[test]
fn a_global_const_statement_declares_its_bare_name() {
    assert_eq!(decls("<?php\nconst FOO = 1;\n"), ["FOO"]);
}

#[test]
fn a_const_statement_declares_into_its_namespace() {
    assert_eq!(decls("<?php\nnamespace App;\nconst FOO = 1;\n"), ["app\\FOO"]);
}

#[test]
fn a_multi_item_const_statement_declares_each_item() {
    assert_eq!(decls("<?php\nconst A = 1, B = 2;\n"), ["A", "B"]);
}

#[test]
fn a_literal_define_declares_an_absolute_name() {
    // `define()` ignores the current namespace: inside `namespace App;`,
    // `define('FOO', 1)` declares the GLOBAL `FOO` (witnessed: `namespace App;
    // define('G','g'); echo G;` prints `g`, and `defined('App\G')` is false).
    assert_eq!(decls("<?php\nnamespace App;\ndefine('FOO', 1);\n"), ["FOO"]);
    assert_eq!(decls("<?php\ndefine('Ns\\\\FOO', 1);\n"), ["ns\\FOO"]);
}

#[test]
fn a_literal_concatenation_is_still_a_literal_name() {
    assert_eq!(decls("<?php\ndefine('PRE' . '_FIX', 1);\n"), ["PRE_FIX"]);
}

#[test]
fn a_class_constant_is_not_a_global_declaration() {
    assert!(decls("<?php\nclass W { const SIZE = 1; }\n").is_empty());
    assert!(decls("<?php\ninterface I { const K = 1; }\n").is_empty());
    assert!(decls("<?php\nenum E { const K = 1; case A; }\n").is_empty());
}

// The computed-`define()` dam site.

fn define_dam_sites(src: &str) -> usize {
    SourceTree::parse(src)
        .dynamism_sites()
        .iter()
        .filter(|s| s.kind == DynamismKind::DefineDynamic)
        .count()
}

#[test]
fn a_computed_define_name_is_a_dam_site_and_not_a_declaration() {
    for src in [
        "<?php\ndefine($name, 1);\n",
        "<?php\ndefine('PRE_' . $suffix, 1);\n",
        "<?php\ndefine(make_name(), 1);\n",
        // A named or spread first argument is not read, so it dams too.
        "<?php\ndefine(...$args);\n",
    ] {
        assert_eq!(define_dam_sites(src), 1, "{src}");
        assert!(decls(src).is_empty(), "{src}");
    }
}

#[test]
fn a_literal_define_is_not_a_dam_site() {
    assert_eq!(define_dam_sites("<?php\ndefine('FOO', 1);\n"), 0);
}

#[test]
fn a_namespaced_define_is_a_different_function_entirely() {
    // `Foo\define(...)` is not the global `define`, so it neither declares nor dams
    // — the same rule `classify_class_alias` applies to `Foo\class_alias`.
    let src = "<?php\nnamespace App;\nFoo\\define($x, 1);\n";
    assert_eq!(define_dam_sites(src), 0);
    assert!(decls(src).is_empty());
}

// Fetches.

#[test]
fn bare_constant_fetches_are_collected_in_every_spelling() {
    assert_eq!(fetches("<?php\necho FOO, \\BAR, Ns\\BAZ;\n"), ["FOO", "BAR", "Ns\\BAZ"]);
}

#[test]
fn the_reserved_literals_and_magic_constants_are_not_fetches() {
    assert!(fetches("<?php\nvar_dump(true, FALSE, Null);\n").is_empty());
    assert!(fetches("<?php\necho __LINE__, __FILE__, __DIR__, __CLASS__, __NAMESPACE__;\n").is_empty());
}

#[test]
fn a_class_constant_access_is_not_a_bare_fetch() {
    assert!(fetches("<?php\necho W::SIZE, W::class;\n").is_empty());
}

// `use const` imports.

/// The namespace context covering a byte offset just inside `needle`.
fn ctx_at<'a>(tree: &'a SourceTree, src: &str, needle: &str) -> &'a steins_syntax::NsCtx {
    let off = (src.find(needle).expect("needle present") + 1) as u32;
    tree.ctx_at(off)
}

#[test]
fn a_plain_use_const_binds_an_exact_case_alias() {
    let src = "<?php\nuse const Other\\FOO;\necho FOO;\n";
    let tree = SourceTree::parse(src);
    let ctx = ctx_at(&tree, src, "echo FOO");
    assert_eq!(ctx.const_imports.get("FOO").map(String::as_str), Some("Other\\FOO"));
    // Case-sensitive (no lowercased alias) and const-scoped (no leak into fn/class maps).
    assert!(!ctx.const_imports.contains_key("foo"));
    assert!(ctx.fn_imports.is_empty() && ctx.class_imports.is_empty());
}

#[test]
fn a_use_const_alias_form_binds_the_alias() {
    let src = "<?php\nuse const Other\\FOO as BAR;\necho BAR;\n";
    let tree = SourceTree::parse(src);
    assert_eq!(
        ctx_at(&tree, src, "echo BAR").const_imports.get("BAR").map(String::as_str),
        Some("Other\\FOO")
    );
}

#[test]
fn a_grouped_use_const_binds_each_item() {
    let src = "<?php\nuse const App\\{A, Sub\\B};\necho A, B;\n";
    let tree = SourceTree::parse(src);
    let ctx = ctx_at(&tree, src, "echo A");
    assert_eq!(ctx.const_imports.get("A").map(String::as_str), Some("App\\A"));
    assert_eq!(ctx.const_imports.get("B").map(String::as_str), Some("App\\Sub\\B"));
}

#[test]
fn a_mixed_group_sorts_each_item_into_its_own_map() {
    let src = "<?php\nuse App\\{Model, function helper, const LIMIT};\necho LIMIT;\n";
    let tree = SourceTree::parse(src);
    let ctx = ctx_at(&tree, src, "echo LIMIT");
    assert_eq!(ctx.class_imports.get("model").map(String::as_str), Some("App\\Model"));
    assert_eq!(ctx.fn_imports.get("helper").map(String::as_str), Some("App\\helper"));
    assert_eq!(ctx.const_imports.get("LIMIT").map(String::as_str), Some("App\\LIMIT"));
}
