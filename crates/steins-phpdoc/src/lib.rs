//! `steins-phpdoc` — a PHPDoc type-expression parser, normatively
//! **phpstan/phpdoc-parser-compatible** (ADR-0029), with type-operation
//! semantics governed by ADR-0030. A PHPDoc type is an authoritative envelope
//! (ADR-0001), so a misparse is a wrong contract — a false-positive vector.
//! Compatibility is enforced mechanically by the oracle harness
//! (`harness/phpdoc-oracle`, `cargo xtask phpdoc-oracle`), which diffs this
//! crate's output against the real phpstan/phpdoc-parser on the same inputs.
//!
//! # Design
//!
//! - [`lexer`]: hand-written, reproduces the reference token stream.
//! - [`parser`]: recursive-descent, reproduces the reference algorithm
//!   including its whitespace-sensitive and save-point/backtrack subtleties.
//! - [`ast`]: own spanned AST; [`std::fmt::Display`] renders the canonical form
//!   matching phpdoc-parser's `__toString()` — what the oracle compares.
//! - [`docblock`]: thin scanner extracting typed tags with positions.
//!
//! # Subset & safety
//!
//! An unaccepted construct yields [`ParseError`]; a deliberately-opaque one
//! yields [`TypeKind::Unsupported`]. Callers treat both as "no envelope" —
//! silence, the safe default. The parser never panics.
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
pub use docblock::{
    AssertKind, DocTag, EnvelopeTag, MagicMemberTag, MagicTagKind, PurityCondition, TagKind,
    TemplateDecl, Variance, scan_docblock, scan_inheritance_args, scan_magic_member_tags,
    scan_template_decls, scan_template_names,
};
pub use parser::{ParseError, TypeParse, parse_type};

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trips representative types through parse + canonical render; guards
    /// the headline grammar features. Exhaustive check: `tests/reference_corpus.rs`.
    #[test]
    fn canonical_forms() {
        let cases = [
            ("int", "int"),
            // These scalars are plain identifiers to the grammar (no special node).
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
