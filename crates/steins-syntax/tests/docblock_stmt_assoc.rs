//! Tests for statement-level docblock association (ADR-0073, issue #93): the
//! line-leading `/** … */` trivium directly above a statement — whitespace-only
//! gap with at most one line break — is adopted via `SourceTree::stmt_docblock`;
//! a blank line, a trailing (non-line-leading) placement, intervening code or
//! comment, or a declaration's own adoption refuses it. Placement only: no tag
//! inside the block is recognized this slice.

use steins_syntax::ScopeOwner;
use steins_syntax::SourceTree;

/// The byte offset where `needle` first occurs in `src` — the statement-start
/// key `stmt_docblock` is queried with (a lowered `Stmt`'s span starts at the
/// statement's first token).
fn at(src: &str, needle: &str) -> u32 {
    u32::try_from(src.find(needle).expect("needle present")).unwrap()
}

#[test]
fn leading_docblock_is_adopted_by_the_next_statement() {
    let src = "<?php\n$a = 0;\n/** @psalm-trace $x */\n$x = 1;\n";
    let tree = SourceTree::parse(src);
    let (text, span) = tree.stmt_docblock(at(src, "$x = 1")).expect("docblock adopted");
    assert!(text.contains("@psalm-trace $x"));
    // The text is the exact source substring at the span, so a docblock-relative
    // tag offset maps into the file by adding `span.start`.
    assert_eq!(&src[span.start as usize..span.end as usize], text);
    // The earlier statement adopts nothing — the block is below it.
    assert!(tree.stmt_docblock(at(src, "$a = 0")).is_none());
}

#[test]
fn first_statement_of_the_file_adopts_its_leading_docblock() {
    let src = "<?php\n/** @psalm-trace $x */\n$x = 1;\n";
    let tree = SourceTree::parse(src);
    assert!(tree.stmt_docblock(at(src, "$x = 1")).is_some());
}

#[test]
fn a_real_trace_statement_span_is_the_query_key() {
    // The seam is consumed with `Stmt::span.start` from the lowered trace, so
    // pin that the top-level statement's span keys the same association the
    // textual offset does.
    let src = "<?php\n/** @psalm-trace $x */\n$x = 1;\n";
    let tree = SourceTree::parse(src);
    let scope = tree
        .scopes()
        .iter()
        .find(|s| s.owner == ScopeOwner::TopLevel)
        .expect("top-level scope");
    let stmt = &scope.stmts[0];
    assert_eq!(stmt.span.start, at(src, "$x = 1"));
    assert!(tree.stmt_docblock(stmt.span.start).is_some());
}

#[test]
fn a_blank_line_between_docblock_and_statement_refuses() {
    let src = "<?php\n/** detached note */\n\n$x = 1;\n";
    let tree = SourceTree::parse(src);
    assert!(tree.stmt_docblock(at(src, "$x = 1")).is_none(), "blank line breaks association");
}

#[test]
fn intervening_code_refuses() {
    let src = "<?php\n/** floats */\n$y = 2;\n$x = 1;\n";
    let tree = SourceTree::parse(src);
    assert!(tree.stmt_docblock(at(src, "$x = 1")).is_none(), "code between breaks association");
}

#[test]
fn an_intervening_line_comment_refuses() {
    let src = "<?php\n/** floats */\n// note\n$x = 1;\n";
    let tree = SourceTree::parse(src);
    assert!(tree.stmt_docblock(at(src, "$x = 1")).is_none(), "`//` comment breaks association");
}

#[test]
fn an_intervening_hash_comment_refuses() {
    let src = "<?php\n/** floats */\n# note\n$x = 1;\n";
    let tree = SourceTree::parse(src);
    assert!(tree.stmt_docblock(at(src, "$x = 1")).is_none(), "`#` comment breaks association");
}

#[test]
fn only_the_adjacent_of_two_consecutive_docblocks_adopts() {
    let src = "<?php\n/** first */\n/** second */\n$x = 1;\n";
    let tree = SourceTree::parse(src);
    let (text, _) = tree.stmt_docblock(at(src, "$x = 1")).expect("adjacent block adopted");
    assert!(text.contains("second"));
    assert!(!text.contains("first"), "only the nearest block associates");
}

#[test]
fn nested_statements_adopt_their_leading_docblocks() {
    // The query is keyed by statement start alone, so depth is immaterial — pin
    // it across the control-flow shapes the trace models or scans.
    let src = "<?php\n\
        if ($c) {\n    /** in-if */\n    $a = 1;\n} else {\n    /** in-else */\n    $b = 2;\n}\n\
        try {\n    /** in-try */\n    $d = 3;\n} catch (Exception $e) {\n    /** in-catch */\n    $f = 4;\n}\n\
        while ($c) {\n    /** in-while */\n    $g = 5;\n}\n\
        switch ($c) {\n    case 1:\n        /** in-case */\n        $h = 6;\n        break;\n}\n\
        function nested() {\n    /** in-fn */\n    $i = 7;\n}\n";
    let tree = SourceTree::parse(src);
    for (needle, tag) in [
        ("$a = 1", "in-if"),
        ("$b = 2", "in-else"),
        ("$d = 3", "in-try"),
        ("$f = 4", "in-catch"),
        ("$g = 5", "in-while"),
        ("$h = 6", "in-case"),
        ("$i = 7", "in-fn"),
    ] {
        let (text, _) = tree
            .stmt_docblock(at(src, needle))
            .unwrap_or_else(|| panic!("{needle} adopts its {tag} docblock"));
        assert!(text.contains(tag), "{needle} adopted the wrong block: {text}");
    }
}

#[test]
fn a_function_declarations_docblock_is_never_statement_adopted() {
    // A `function` declaration is itself a statement; its docblock belongs to
    // the declaration channel (ADR-0029) and the statement channel refuses it
    // (ADR-0073 exclusivity) — one docblock, one owner.
    let src = "<?php\n/** @param int $n */\nfunction f($n): void {}\n";
    let tree = SourceTree::parse(src);
    assert!(tree.functions()[0].docblock.is_some(), "declaration adoption unchanged");
    assert!(tree.stmt_docblock(at(src, "function f")).is_none(), "declaration owns the block");
}

#[test]
fn a_class_declarations_docblock_is_never_statement_adopted() {
    let src = "<?php\n/** @template T */\nclass C {}\n";
    let tree = SourceTree::parse(src);
    assert!(tree.classes()[0].docblock.is_some(), "declaration adoption unchanged");
    assert!(tree.stmt_docblock(at(src, "class C")).is_none(), "declaration owns the block");
}

#[test]
fn member_docblocks_are_never_statement_adopted() {
    let src = "<?php\nclass C {\n    /** @var int */\n    public $p = 1;\n\n    /** @return void */\n    public function m(): void {\n        $x = 1;\n    }\n}\n";
    let tree = SourceTree::parse(src);
    let c = &tree.classes()[0];
    assert!(c.properties[0].docblock.is_some(), "property adoption unchanged");
    assert!(c.methods[0].docblock.is_some(), "method adoption unchanged");
    assert!(tree.stmt_docblock(at(src, "public $p")).is_none());
    assert!(tree.stmt_docblock(at(src, "public function m")).is_none());
    // The method body's own first statement still adopts nothing — the method
    // head sits between it and the method's docblock.
    assert!(tree.stmt_docblock(at(src, "$x = 1")).is_none());
}

#[test]
fn a_trailing_docblock_is_adopted_by_neither_neighbor() {
    // A block trailing another statement's line is not line-leading, so it
    // associates with nothing (ADR-0073 placement rule): never backward to the
    // statement it trails, and never forward to the next statement either.
    let src = "<?php\n$x = 1; /** trailing */\n$y = 2;\n";
    let tree = SourceTree::parse(src);
    assert!(tree.stmt_docblock(at(src, "$x = 1")).is_none(), "a block after the start never associates backward");
    assert!(tree.stmt_docblock(at(src, "$y = 2")).is_none(), "a trailing block never associates forward");
}

#[test]
fn a_line_leading_docblock_adopts_its_same_line_statement() {
    // The leading form written inline: the block leads its line and the
    // statement follows on the same line — unambiguous, so it adopts.
    let src = "<?php\n/** @psalm-trace $x */ $x = 1;\n";
    let tree = SourceTree::parse(src);
    let (text, _) = tree.stmt_docblock(at(src, "$x = 1")).expect("inline leading form adopts");
    assert!(text.contains("@psalm-trace $x"));
}

#[test]
fn a_docblock_at_eof_adopts_nothing() {
    let src = "<?php\n$x = 1;\n/** dangling */\n";
    let tree = SourceTree::parse(src);
    assert!(tree.parse_errors().is_empty());
    assert!(tree.stmt_docblock(at(src, "$x = 1")).is_none(), "the dangling block follows the only statement");
}
