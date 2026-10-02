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
