//! Steins' syntax-tree contract and its Mago parser backend (ADR-0003).
//!
//! # Encapsulation (hard rule)
//!
//! The pinned Mago fork is a dependency of *this crate only* — **no Mago type
//! appears in this crate's public API**. Everything the analyzer sees is the
//! owned, lowered representation here ([`SourceTree`] and its plain-data structs),
//! the seam ADR-0003 requires so parser backends can be swapped freely. Spans are
//! byte offsets, convertible to 1-based line/column via [`SourceTree::position`].

use mago_syntax::cst::{Node, Trivia, TriviaKind};

mod ast;
mod lower_decl;
mod lower_effect;
mod lower_expr;
mod lower_presence;
mod lower_scope;
mod lower_stmt;
mod names;
pub mod stack_guard;
mod tree;

pub use ast::*;
pub use lower_expr::php_canonical_int_string;
pub use tree::SourceTree;

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

fn to_span(span: mago_span::Span) -> Span {
    Span { start: span.start.offset, end: span.end.offset }
}

/// The children of `node`, **or none when the stack is spent** (issue #264).
///
/// Every walker in this file descends through here, so this one function is the
/// whole depth guard for the CST walk: when [`stack_guard::exhausted`] says the
/// remaining headroom is gone, a walker is handed an empty child list and returns
/// the way it would at a leaf. No walker's control flow changes or unwinds, and
/// the parse still produces a (partial) tree, which [`SourceTree::parse`] then
/// reports as a recovered parse error rather than letting the process (or the
/// wasm module) die walking it.
///
/// On every native target the guard is off by default and this is
/// `node.children()` behind one thread-local read; see [`stack_guard`].
fn children<'ast, 'arena>(node: &Node<'ast, 'arena>) -> Vec<Node<'ast, 'arena>> {
    if stack_guard::exhausted() {
        return Vec::new();
    }
    node.children()
}

/// Lower one trivium to a [`Comment`], dropping whitespace trivia (`None`).
fn lower_comment(t: &Trivia<'_>) -> Option<Comment> {
    let kind = match t.kind {
        TriviaKind::SingleLineComment => CommentKind::Line,
        TriviaKind::HashComment => CommentKind::Hash,
        TriviaKind::MultiLineComment => CommentKind::Block,
        TriviaKind::DocBlockComment => CommentKind::DocBlock,
        TriviaKind::WhiteSpace => return None,
    };
    Some(Comment { kind, span: to_span(t.span), text: bytes_to_string(t.value) })
}

fn bytes_to_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn strip_dollar(name: String) -> String {
    name.strip_prefix('$').map_or(name.clone(), ToOwned::to_owned)
}

fn line_starts(source: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i as u32 + 1);
        }
    }
    starts
}
