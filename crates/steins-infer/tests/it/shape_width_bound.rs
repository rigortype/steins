//! ADR-0062 Amendment M — the width bound holds at the constructor, for every
//! producer (issue #884).
//!
//! A-G6 bounded a shape at 256 fields where an array *literal* is lifted. An
//! array the analysis builds itself, one `$a[] = v` or `$a['k'] = v` at a time,
//! grew a sealed shape without limit, and the rebuild per statement was
//! quadratic (the time budgets are in the CLI suite). The bound now lives in
//! `ShapeFact::normalize_counted`, so those producers degrade to the tail-only
//! summary exactly as a 300-entry literal does.
//!
//! The suite pins the **one semantic movement**, at the line where it happens:
//! after 256 appends the key list is still there, after 300 it is not, so a
//! read of the first missing integer index stops being reported. Reading a key
//! of the other class is still reported, because the summary keeps the key
//! class.

use std::collections::HashMap;

use steins_domain::Fact;
use steins_infer::{DEBUG_TYPE_ID, Folder, OFFSET_UNDECLARED_ID, check_with};
use steins_syntax::{ArgValue, SourceTree};

#[derive(Default)]
struct Mock(HashMap<String, Fact>);

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.0.get(&name.to_ascii_lowercase()).cloned()
    }
}

/// `function f()` building `$a = [];` with one statement per `line(i)`, then
/// `tail`.
fn source(n: usize, line: impl Fn(usize) -> String, tail: &str) -> String {
    let mut src = String::from("<?php\nfunction f(): void {\n$a = [];\n");
    for i in 0..n {
        src.push_str(&line(i));
        src.push('\n');
    }
    src.push_str(tail);
    src.push_str("\n}\n");
    src
}

fn appends(n: usize, tail: &str) -> String {
    source(n, |i| format!("$a[] = {i};"), tail)
}

fn writes(n: usize, tail: &str) -> String {
    source(n, |i| format!("$a['k{i}'] = {i};"), tail)
}

fn messages_with(src: &str, id: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Mock::default())
        .into_iter()
        .filter(|d| d.id == id)
        .map(|d| d.message)
        .collect()
}

/// The `offset.undeclared` messages, shortened to the part naming the key.
fn undeclared(src: &str) -> Vec<String> {
    messages_with(src, OFFSET_UNDECLARED_ID)
        .into_iter()
        .map(|m| m.split(" is outside").next().unwrap_or(&m).to_owned())
        .collect()
}

fn dumped(src: &str) -> String {
    let dumps = messages_with(src, DEBUG_TYPE_ID);
    assert_eq!(dumps.len(), 1, "expected one dump, got {dumps:?}");
    dumps[0].clone()
}

#[test]
fn at_the_bound_the_key_list_is_still_there() {
    // 256 appends are keys `0..=255`: the shape is listed, so the first index
    // past it is proven missing and `$a[256]` is reported.
    let src = appends(256, "$x = $a[255]; $y = $a[256];");
    assert_eq!(undeclared(&src), ["offset 256"]);
}

#[test]
fn the_dump_past_the_bound_is_the_tail_summary() {
    let src = appends(300, "\\PHPStan\\dumpType($a);");
    assert_eq!(dumped(&src), "dumped type: non-empty-array<int, int<0, 299>>");
}

#[test]
fn past_the_bound_the_first_missing_index_stops_being_reported() {
    // The pinned movement. Keys are `0..=299`, so `$a[299]` is present on
    // master and now, `$a[300]` was `offset.undeclared` on master and is silent
    // with the cap, and `$a['k']` is reported on both: the summary's key class
    // is `int`, which cannot carry a string key.
    let src = appends(300, "$x = $a[299]; $y = $a[300]; $z = $a['k'];");
    assert_eq!(undeclared(&src), ["offset 'k'"]);
}

#[test]
fn string_keyed_writes_cross_the_bound_the_same_way() {
    // 300 writes at `'k0'..='k299'`: the key class is `string`, so an integer
    // read is still reported, and the first missing string key is not.
    let src = writes(300, "$x = $a['k299']; $y = $a['k300']; $z = $a[0];");
    assert_eq!(undeclared(&src), ["offset 0"]);
    // The earliest keys went into the tail summary when the 257th arrived; the
    // writes after it list again, up to the bound, on top of that tail.
    let dump = dumped(&writes(300, "\\PHPStan\\dumpType($a);"));
    assert!(dump.contains("...<string, "), "the summary is the tail: {dump}");
    assert!(!dump.contains("k0:"), "the first key is no longer listed: {dump}");
}

#[test]
fn writes_at_the_bound_keep_the_key_list() {
    let src = writes(256, "$x = $a['k255']; $y = $a['k256'];");
    assert_eq!(undeclared(&src), ["offset 'k256'"]);
}

// A write that adds a key does not carry list-ness (issue #884, review).
//
// Degrading at the 257th write keeps `is_list` at `Yes`, which is true of the
// keys `0..=256` written in order. The next write at a gap then took the
// unwitnessed path, whose `promote_present` handed the receiver's flag on, and
// against an unsealed integer tail the denotational verdict is `Maybe`, so the
// stale `Yes` survived: `{1000: 'x', ...}` rendered as a non-empty list, was
// rejected by an `associative-array` declaration and folded `array_is_list`
// to true. PHP says false. The same defect was on master for a declared
// `non-empty-list` and for a 300-entry literal; the bound only made it easy
// to reach.

fn int_writes(n: usize, tail: &str) -> String {
    source(n, |i| format!("$a[{i}] = {i};"), tail)
}

/// Every diagnostic a source produces whose id is in the `phpdoc.` family.
fn phpdoc_findings(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Mock::default())
        .into_iter()
        .filter(|d| d.id.starts_with("phpdoc."))
        .map(|d| format!("{}: {}", d.id, d.message))
        .collect()
}

#[test]
fn a_gap_write_after_the_bound_leaves_a_non_list() {
    let dump = dumped(&int_writes(257, "$a[1000] = 'x'; \\PHPStan\\dumpType($a);"));
    assert!(!dump.contains("list"), "a gap write makes no list: {dump}");
}

#[test]
fn a_gap_write_after_the_bound_satisfies_an_associative_declaration() {
    let mut src = String::from(
        "<?php\n/** @return associative-array<int, int|string> */\nfunction g(): array {\n$a = [];\n",
    );
    for i in 0..257 {
        src.push_str(&format!("$a[{i}] = {i};\n"));
    }
    src.push_str("$a[1000] = 'x';\nreturn $a;\n}\n");
    src.push_str("/** @param associative-array<int, int|string> $x */\n");
    src.push_str("function take(array $x): void {}\n");
    src.push_str("function h(): void { $b = g(); take($b); }\n");
    assert_eq!(phpdoc_findings(&src), Vec::<String>::new());
}

#[test]
fn a_gap_write_on_a_declared_list_leaves_a_non_list() {
    let src = "<?php\n/** @param non-empty-list<int> $d */\n\
               function f(array $d): void { $d[1000] = 5; \\PHPStan\\dumpType($d); }\n";
    let dump = dumped(src);
    assert!(!dump.contains("list"), "a gap write makes no list: {dump}");
}

#[test]
fn a_gap_write_on_a_wide_literal_leaves_a_non_list() {
    let items: Vec<String> = (0..300).map(|i| i.to_string()).collect();
    let src = format!(
        "<?php\nfunction f(): void {{ $a = [{}]; $a[1000] = 1; \\PHPStan\\dumpType($a); }}\n",
        items.join(", ")
    );
    let dump = dumped(&src);
    assert!(!dump.contains("list"), "a gap write makes no list: {dump}");
}

#[test]
fn overwriting_a_key_the_list_already_has_keeps_it_a_list() {
    let src = "<?php\nfunction f(): void { $a = [1, 2, 3]; $a[1] = 9; \\PHPStan\\dumpType($a); }\n";
    assert_eq!(dumped(src), "dumped type: list{1, 9, 3}");
}

// A write a proven list cannot make a gap in keeps the list: a key below the
// count floor overwrites, and the floor itself can only be the next index.

fn dump_of(decl: &str, body: &str) -> String {
    dumped(&format!(
        "<?php\n/** @param {decl} $a */\nfunction f(array $a, bool $c): void {{ {body} \\PHPStan\\dumpType($a); }}\n"
    ))
}

#[test]
fn a_write_at_the_first_index_of_a_declared_list_keeps_it_a_list() {
    let dump = dump_of("list<int>", "$a[0] = 5;");
    assert!(dump.starts_with("dumped type: non-empty-list"), "{dump}");
    let dump = dump_of("non-empty-list<int>", "$a[0] = 5;");
    assert!(dump.starts_with("dumped type: non-empty-list"), "{dump}");
}

#[test]
fn a_write_at_the_next_index_of_a_maybe_extended_list_keeps_it_a_list() {
    let src = "<?php\nfunction f(bool $c): void { $a = [1, 2]; if ($c) { $a[] = 3; } $a[2] = 9; \
               \\PHPStan\\dumpType($a); }\n";
    let dump = dumped(src);
    assert!(dump.contains("list"), "{dump}");
}

#[test]
fn a_write_at_zero_after_an_unset_keeps_a_one_element_list() {
    let src = "<?php\nfunction f(): void { $a = [1]; unset($a[0]); $a[0] = 2; \
               \\PHPStan\\dumpType($a); }\n";
    let dump = dumped(src);
    assert!(dump.contains("list"), "{dump}");
}

#[test]
fn a_write_past_the_count_floor_of_a_declared_list_is_still_a_gap() {
    // The floor is 1: key 1 is an overwrite or the next index, key 2 may be a gap.
    let dump = dump_of("non-empty-list<int>", "$a[1] = 5;");
    assert!(dump.contains("list"), "{dump}");
    let dump = dump_of("non-empty-list<int>", "$a[2] = 5;");
    assert!(!dump.contains("list"), "{dump}");
}

// A docblock shape wider than the bound goes through the same constructor, so
// it degrades too: the contract-lowering reach of Amendment M.

#[test]
fn a_declared_shape_past_the_bound_is_summarized_too() {
    let fields: Vec<String> = (0..300).map(|i| format!("k{i}: int")).collect();
    let src = format!(
        "<?php\n/** @param array{{{}}} $d */\nfunction f(array $d): void {{ $x = $d['nope']; }}\n",
        fields.join(", ")
    );
    // Master reports `'nope'` against the sealed 300-key shape; the summary's
    // key class is `string`, which admits it.
    assert_eq!(undeclared(&src), Vec::<String>::new());
    let narrow = "<?php\n/** @param array{k0: int, k1: int} $d */\n\
                  function f(array $d): void { $x = $d['nope']; }\n";
    assert_eq!(undeclared(narrow), ["offset 'nope'"]);
}
