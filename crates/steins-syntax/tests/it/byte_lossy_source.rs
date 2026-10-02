//! A source file that is not valid UTF-8 (issue #927, ADR-0080 §3.2 interim): the decode
//! replaces each ill-formed sequence with U+FFFD, and the lowering must not let two
//! different byte strings, or two different names, collapse into the one text that results.

use steins_domain::PhpStr;
use steins_syntax::{ArgValue, ArrayKey, SourceTree, decode_source};

/// Decode `bytes` the way every loader does and parse the result with its loss map.
fn parse_bytes(bytes: &[u8]) -> SourceTree {
    let (text, loss) = decode_source(bytes.to_vec());
    SourceTree::parse_with_loss(&text, loss.as_ref())
}

/// The first positional argument of the file's first call.
fn first_arg(tree: &SourceTree) -> ArgValue {
    tree.calls()[0].args[0].value.clone()
}

fn string_of(v: &ArgValue) -> &PhpStr {
    match v {
        ArgValue::Str(s) => s,
        other => panic!("expected a string, got {other:?}"),
    }
}

#[test]
fn a_literal_over_an_ill_formed_byte_is_the_bytes_the_file_spells() {
    let tree = parse_bytes(b"<?php f(\"\x82\xA0\");");
    let v = first_arg(&tree);
    let s = string_of(&v);
    assert_eq!(s.as_bytes(), b"\x82\xA0");
    assert!(!s.is_utf8(), "ill-formed bytes are the byte-string arm, so name lanes decline");
}

#[test]
fn two_literals_differing_only_in_the_replaced_bytes_are_two_values() {
    let a = parse_bytes(b"<?php f(\"\x82\xA0\");");
    let b = parse_bytes(b"<?php f(\"\x82\xA2\");");
    assert_ne!(first_arg(&a), first_arg(&b));
    // The base lowering gave both the same `"\u{FFFD}\u{FFFD}"`; that is the defect.
    let (text_a, _) = decode_source(b"<?php f(\"\x82\xA0\");".to_vec());
    let (text_b, _) = decode_source(b"<?php f(\"\x82\xA2\");".to_vec());
    assert_eq!(text_a, text_b);
    // And equal bytes stay equal.
    let c = parse_bytes(b"<?php f(\"\x82\xA0\");");
    assert_eq!(first_arg(&a), first_arg(&c));
}

#[test]
fn the_escapes_around_a_replaced_byte_are_the_parsers_own() {
    // `\n`, a hex escape, an octal escape and an escaped backslash, either side of a byte.
    let tree = parse_bytes(b"<?php f(\"\\n\x82\\x41\\101\\\\\x83z\");");
    assert_eq!(string_of(&first_arg(&tree)).as_bytes(), b"\n\x82AA\\\x83z");
    // A single-quoted literal knows only `\\` and `\'`.
    let tree = parse_bytes(b"<?php f('\\'\x82\\n\\\\');");
    assert_eq!(string_of(&first_arg(&tree)).as_bytes(), b"'\x82\\n\\");
}

#[test]
fn a_physical_byte_and_a_genuine_replacement_character_stay_two_things() {
    // `"\u{FFFD}<0x82>"` and `"<0x82>\u{FFFD}"` differ only in order; a literal U+FFFD
    // written as an escape is three valid bytes and must not be mistaken for the loss point.
    let a = parse_bytes(b"<?php f(\"\\u{FFFD}\x82\");");
    let b = parse_bytes(b"<?php f(\"\x82\\u{FFFD}\");");
    assert_eq!(string_of(&first_arg(&a)).as_bytes(), b"\xEF\xBF\xBD\x82");
    assert_eq!(string_of(&first_arg(&b)).as_bytes(), b"\x82\xEF\xBF\xBD");
    assert_ne!(first_arg(&a), first_arg(&b));
    // The same written as the physical three bytes of U+FFFD beside a stray one.
    let c = parse_bytes(b"<?php f(\"\xEF\xBF\xBD\x82\");");
    assert_eq!(first_arg(&a), first_arg(&c));
}

#[test]
fn a_valid_multibyte_neighbour_is_kept_and_bytes_that_join_into_it_are_valid() {
    let tree = parse_bytes(b"<?php f(\"\xE3\x81\x82\x82\xE3\x81\x84\");");
    assert_eq!(string_of(&first_arg(&tree)).as_bytes(), b"\xE3\x81\x82\x82\xE3\x81\x84");
    // Escapes can spell a lead byte that the raw continuation then completes: the value is
    // `あ`, valid UTF-8, and lowers as the ordinary arm.
    let tree = parse_bytes(b"<?php f(\"\\xE3\\x81\x82\");");
    let v = first_arg(&tree);
    assert_eq!(string_of(&v).as_str(), Some("\u{3042}"));
}

#[test]
fn the_literal_parts_of_an_interpolated_string_read_the_map_too() {
    let tree = parse_bytes(b"<?php f(\"\x82$x\\t\x83\");");
    // `"" . "<82>" . $x . "\t<83>"`, left-nested.
    let ArgValue::Concat(l, r) = first_arg(&tree) else { panic!("an interpolation is a chain") };
    assert_eq!(string_of(&r).as_bytes(), b"\t\x83");
    let ArgValue::Concat(l, _) = *l else { panic!("a chain") };
    let ArgValue::Concat(_, first) = *l else { panic!("a chain") };
    assert_eq!(string_of(&first).as_bytes(), b"\x82");
}

#[test]
fn array_keys_compare_by_bytes() {
    let tree = parse_bytes(b"<?php f(['\x82\xA0' => 1, '\x82\xA2' => 2]);");
    let ArgValue::Array(items) = first_arg(&tree) else { panic!("an array literal") };
    let (ArrayKey::Str(a), ArrayKey::Str(b)) = (&items[0].0, &items[1].0) else {
        panic!("string keys")
    };
    assert_ne!(a, b);
}

#[test]
fn a_name_over_a_replaced_byte_marks_the_tree() {
    for src in [
        &b"<?php class Bag { public int $field\xC9 = 1; }"[..],
        b"<?php function f\xC9() {}",
        b"<?php class C { function m\xC9() {} }",
        b"<?php class C\xC9 {}",
        b"<?php $v\xC9 = 1;",
        b"<?php $o->p\xC9 = 1;",
        b"<?php const K\xC9 = 1;",
        b"<?php use Foo\\Bar\xC9;",
        b"<?php #[Attr\xC9] function g() {}",
        // A string read as a name.
        b"<?php array_map('cb\xC9', []);",
    ] {
        assert!(parse_bytes(src).names_lossy(), "{}", String::from_utf8_lossy(src));
    }
}

#[test]
fn a_replaced_byte_that_is_no_name_leaves_the_names_alone() {
    for src in [
        &b"<?php // \x82\xA0 a comment\nclass Bag { public int $x = 1; }"[..],
        b"<?php /** @param int $x \x82\xA0 the count */ function f($x) {}",
        b"<?php $s = \"\x82\xA0\"; $t = ['\x82' => 1]; echo $s;",
        b"<html>\x82\xA0</html><?php class A {}",
        b"<?php class A {} ?>\n\x82\xA0 trailing text",
    ] {
        assert!(!parse_bytes(src).names_lossy(), "{}", String::from_utf8_lossy(src));
    }
}

#[test]
fn a_valid_file_is_untouched_by_a_genuine_replacement_character() {
    // A U+FFFD in a name or a literal of a file that is valid UTF-8 is an ordinary
    // character: no loss map, no mark, and the literal keeps its `Utf8` arm.
    let tree = SourceTree::parse("<?php class A\u{FFFD} { public $p\u{FFFD}; } f(\"\u{FFFD}\");");
    assert!(!tree.names_lossy());
    assert_eq!(string_of(&first_arg(&tree)).as_str(), Some("\u{FFFD}"));
}
