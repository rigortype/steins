//! `steins-phpdoc` — a PHPDoc type-expression parser whose grammar is normatively
//! **phpstan/phpdoc-parser-compatible** (ADR-0029), with the type-operation
//! semantics governed by ADR-0030.
//!
//! Because a PHPDoc type becomes an authoritative envelope (ADR-0001), a
//! misparsed docblock is a wrong contract — a false-positive vector. So the
//! grammar here is a faithful, hand-written port of the de-facto standard parser,
//! and compatibility is enforced mechanically by the oracle harness
//! (`harness/phpdoc-oracle`, `cargo xtask phpdoc-oracle`): the same inputs run
//! through the *real* phpstan/phpdoc-parser and are diffed against this crate.
//!
//! # Design
//!
//! - A hand-written [`lexer`] reproducing the reference token stream, and a
//!   recursive-descent [`parser`] reproducing its algorithm (including the
//!   whitespace-sensitive and save-point/backtrack subtleties that decide
//!   compatibility). See those modules for the port notes.
//! - An own, spanned [`ast`]. [`std::fmt::Display`] renders the **canonical
//!   form** matching phpdoc-parser's node `__toString()` — the string the oracle
//!   compares against.
//! - A thin [`docblock`] scanner that extracts typed tags with positions — the
//!   eventual integration seam with `steins-syntax`'s raw comment trivia (not
//!   wired up this phase).
//!
//! # Subset & safety
//!
//! Constructs are implemented in envelope-checking priority order. Anything the
//! parser cannot accept yields a [`ParseError`]; a construct we deliberately keep
//! opaque yields a [`TypeKind::Unsupported`] node. Callers treat **both** as "no
//! envelope" — silence, always the safe side. The parser never panics on input.
//!
//! ```
//! use steins_phpdoc::{parse_type, ast::TypeKind};
//!
//! let parsed = parse_type("array<int, non-empty-string>").unwrap();
//! assert!(parsed.at_end);
//! // Canonical form matches phpstan/phpdoc-parser's __toString.
//! assert_eq!(parsed.ty.to_string(), "array<int, non-empty-string>");
//!
//! // Unions/intersections are always parenthesized in the canonical form.
//! assert_eq!(parse_type("int|string").unwrap().ty.to_string(), "(int | string)");
//!
//! // A `@param` type followed by a variable/description is a partial parse.
//! let p = parse_type("Foo $bar the description").unwrap();
//! assert!(!p.at_end);
//! assert_eq!(p.ty.to_string(), "Foo");
//! ```

pub mod ast;
pub mod docblock;
pub mod lexer;
pub mod parser;

pub use ast::{Type, TypeKind};
pub use docblock::{AssertKind, DocTag, TagKind, scan_docblock, scan_template_names};
pub use parser::{ParseError, TypeParse, parse_type};

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip a handful of representative types through parse + canonical
    /// render. The exhaustive check is the ported reference corpus in
    /// `tests/reference_corpus.rs`; these guard the headline grammar features.
    #[test]
    fn canonical_forms() {
        let cases = [
            ("int", "int"),
            // The open keyword table: these scalars are plain identifiers to the
            // grammar (no special node), so unknown-but-valid names just work.
            ("numeric-string", "numeric-string"),
            ("non-empty-string", "non-empty-string"),
            ("non-falsy-string", "non-falsy-string"),
            ("positive-int", "positive-int"),
            ("negative-int", "negative-int"),
            ("non-negative-int", "non-negative-int"),
            ("array-key", "array-key"),
            ("scalar", "scalar"),
            ("never", "never"),
            ("void", "void"),
            ("int<min, 100>", "int<min, 100>"),
            ("iterable<T>", "iterable<T>"),
            ("?Foo", "?Foo"),
            ("\\App\\User", "\\App\\User"),
            ("int|string|null", "(int | string | null)"),
            ("Foo&Bar", "(Foo & Bar)"),
            ("string[]", "string[]"),
            ("(int|string)[]", "(int | string)[]"),
            ("array<int, string>", "array<int, string>"),
            ("list<Foo>", "list<Foo>"),
            ("non-empty-list<Foo>", "non-empty-list<Foo>"),
            ("array{a: int, b?: string}", "array{a: int, b?: string}"),
            ("array{int, string, ...}", "array{int, string, ...}"),
            ("array{...<string>}", "array{...<string>}"),
            ("object{a: int}", "object{a: int}"),
            ("callable(int, string=): bool", "callable(int, string=): bool"),
            ("\\Closure(T): R", "\\Closure(T): R"),
            ("int<0, max>", "int<0, max>"),
            ("'foo'|'bar'", "('foo' | 'bar')"),
            ("Foo::BAR", "Foo::BAR"),
            ("Foo::*", "Foo::*"),
            ("self::TYPES[int]", "self::TYPES[int]"),
            ("$this", "$this"),
            ("(Foo is Bar ? never : int)", "(Foo is Bar ? never : int)"),
        ];
        for (input, expected) in cases {
            let parsed = parse_type(input)
                .unwrap_or_else(|e| panic!("parse `{input}` failed: {e}"));
            assert!(parsed.at_end, "`{input}` did not fully parse");
            assert_eq!(parsed.ty.to_string(), expected, "canonical for `{input}`");
        }
    }

    /// `__benevolent<T1|T2>` is accepted and expanded to the plain union
    /// `(T1 | T2)`, with provenance retained on the union (ADR-0030).
    #[test]
    fn benevolent_expands_to_union() {
        let parsed = parse_type("__benevolent<int|string>").unwrap();
        assert!(parsed.at_end);
        assert_eq!(parsed.ty.to_string(), "(int | string)");
        match parsed.ty.kind {
            TypeKind::Union { benevolent, .. } => assert!(benevolent),
            other => panic!("expected benevolent union, got {other:?}"),
        }
    }

    /// Invalid input errors rather than panicking, and never yields a type.
    #[test]
    fn invalid_input_errors() {
        assert!(parse_type("array{").is_err());
        assert!(parse_type("Foo<").is_err());
    }
}
