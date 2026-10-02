//! Where the tag scanners split a type from its variable, against the reference parser
//! (issue #932). `doc-tags.txt` holds one docblock tag per line and `doc-tags.expected`
//! the verdict of the real `phpstan/phpdoc-parser`'s `PhpDocParser`
//! (`harness/phpdoc-oracle/dump.php --tags`): the type and the variable the tag
//! declares, or `INVALID` when the tag value does not parse.
//!
//! The reference reads the variable from the token that follows the type. A `$name`
//! inside the description is prose: `@var Foo the result, unlike $other` declares no
//! variable, and `@param int the count $n` is an invalid tag.

use steins_phpdoc::{TagKind, parse_type, scan_docblock, scan_magic_member_tags};

/// What the reference made of one tag line.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// A typed tag. `variable` is empty when it names none; `ty` is empty for a typeless
    /// `@param $x`, which the scanner never emits.
    Tag { ty: String, variable: String },
    Invalid,
}

fn parse_expected(line: &str) -> Verdict {
    if line == "INVALID" {
        return Verdict::Invalid;
    }
    let mut cols = line.splitn(3, '\t');
    assert_eq!(cols.next(), Some("TAG"), "unexpected oracle row `{line}`");
    // `dump.php` escapes a backslash as `\\`, like the reference corpus's `.expected`.
    let ty = cols.next().expect("type column").replace("\\\\", "\\");
    let variable = cols.next().expect("variable column").to_owned();
    Verdict::Tag { ty, variable }
}

fn rows() -> Vec<(&'static str, Verdict)> {
    let inputs = include_str!("../fixtures/doc-tags.txt");
    let expected = include_str!("../fixtures/doc-tags.expected");
    let inputs: Vec<&str> =
        inputs.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    let expected: Vec<Verdict> = expected
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(parse_expected)
        .collect();
    assert_eq!(inputs.len(), expected.len(), "doc-tags.txt and doc-tags.expected are misaligned");
    inputs.into_iter().zip(expected).collect()
}

fn canonical(type_text: &str) -> Option<String> {
    parse_type(type_text).ok().map(|p| p.ty.to_string())
}

/// The scanner's reading of the row as `(type canonical form, variable)`: `None` when the
/// scanner emits no tag.
fn scanned_tag(row: &str) -> Option<(Option<String>, String)> {
    let doc = format!("/** {row} */");
    let tags = scan_docblock(&doc);
    assert!(tags.len() <= 1, "`{row}` scanned to {} tags", tags.len());
    let tag = tags.into_iter().next()?;
    assert!(
        matches!(tag.kind, TagKind::Param | TagKind::Var | TagKind::Assert { .. }),
        "`{row}` scanned to {:?}",
        tag.kind
    );
    Some((canonical(&tag.type_text), tag.var_name.unwrap_or_default()))
}

#[test]
fn tag_scanner_splits_the_variable_where_the_reference_does() {
    let mut failures = Vec::new();
    for (row, verdict) in rows() {
        if row.starts_with("@property") {
            continue;
        }
        let scanned = scanned_tag(row);
        let ok = match (&verdict, &scanned) {
            // A typeless `@param $x` declares no type: nothing to offer.
            (Verdict::Tag { ty, .. }, None) => ty.is_empty(),
            (Verdict::Tag { ty, variable }, Some((scanned_ty, scanned_var))) => {
                !ty.is_empty()
                    && scanned_var == variable
                    && scanned_ty.as_deref().is_none_or(|t| t == ty)
            }
            // An invalid tag declares no variable.
            (Verdict::Invalid, None) => true,
            (Verdict::Invalid, Some((_, var))) => var.is_empty(),
        };
        if !ok {
            failures.push(format!("`{row}`: reference {verdict:?}, scanner {scanned:?}"));
        }
    }
    assert!(failures.is_empty(), "scanner disagrees with the reference:\n{}", failures.join("\n"));
}

#[test]
fn magic_property_scanner_splits_the_variable_where_the_reference_does() {
    let mut checked = 0;
    for (row, verdict) in rows() {
        if !row.starts_with("@property") {
            continue;
        }
        let doc = format!("/** {row} */");
        let tags = scan_magic_member_tags(&doc);
        assert_eq!(tags.len(), 1, "`{row}` scanned to {} tags", tags.len());
        let want = match &verdict {
            Verdict::Tag { variable, .. } => variable.trim_start_matches('$'),
            Verdict::Invalid => "",
        };
        assert_eq!(tags[0].subject, want, "`{row}`: reference {verdict:?}");
        checked += 1;
    }
    assert!(checked >= 4, "the fixture lost its @property rows");
}

/// The scanner's type for a bare `@var` is the parsed prefix, not the prefix and the
/// description with it: the cast seeds `Foo`.
#[test]
fn a_tag_with_no_variable_carries_the_parsed_type_prefix() {
    let doc = "/** @var Foo the result, unlike $other */";
    let tags = scan_docblock(doc);
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].var_name, None);
    assert_eq!(tags[0].type_text, "Foo");
    let s = tags[0].type_span;
    assert_eq!(&doc[s.start as usize..s.end as usize], "Foo");
}
