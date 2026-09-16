//! ADR-0062 §4 — `$a[$i] = v`, the write at a key nobody can name (issue #636).
//!
//! Before this, `$a[$i] = v` never reached [`apply_offset_write`] at all: the
//! lowering gate demanded a literal key, so the statement fell to
//! `StmtKind::Barrier` and the whole environment went with it. It reaches the
//! shape lane now, and the lane answers ADR-0062 §4's weakest sound row.
//!
//! What this suite pins is the *shape* of that answer, and mostly what it
//! REFUSES to say:
//!
//! * the base survives at all — the barrier no longer eats the binding;
//! * `non-empty` holds, because a write leaves an entry behind either way;
//! * **list-ness does not survive**, whatever the base was. `[1, 2, 3]` written
//!   at index `7` has keys `0, 1, 2, 7`, and
//!   `php -r '$a=[1,2,3]; $i=7; $a[$i]=99; var_dump(array_is_list($a));'`
//!   prints `bool(false)` at PHP 8.5.9. The issue's table asked for
//!   `non-empty-list<T|V>` here; PHP says no;
//! * a proven-`Absent` key of the written class comes back as `Optional`, and a
//!   key of the *other* class keeps both its absence proof and its value;
//! * `unset($a[$i])` is NOT in this lane — it keeps the barrier it always had.
//!
//! Zero emission (A-G9): every fixture here dumps and nothing else.
//!
//! [`apply_offset_write`]: ../../steins_infer/shapes/fn.apply_offset_write.html

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// The same mock sidecar the rest of the shape suites use.
#[derive(Default)]
struct Mock {
    facts: HashMap<String, Fact>,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut facts = HashMap::new();
        facts.insert(
            "count".to_owned(),
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false),
        );
        Mock { facts }
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
}

/// The single `debug.type` body a one-dump source produces, asserting on the way
/// that the source produced NO other finding (A-G9).
fn one_type(src: &str) -> String {
    let tree = SourceTree::parse(src);
    let ds: Vec<Diagnostic> = check_with(&tree, &[], "t.php", &mut Mock::sidecar())
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect();
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "an unnamed-key write emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// A body-only fixture with one native `int` parameter to key with.
fn dump_body(body: &str) -> String {
    one_type(&format!("<?php\nfunction f(int $i, string $s): void {{ {body} }}\n"))
}

/// A `@param`-declared fixture: `$v` carries `decl`, `$i` is a native `int`.
fn dump_decl(decl: &str, body: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {decl} $v */\n\
         function f(array $v, int $i, string $s): void {{ {body} }}\n"
    ))
}

// The binding survives at all — the headline

#[test]
fn a_computed_offset_write_no_longer_eats_the_environment() {
    // Before #636 this dumped `unknown`: the statement was a `Barrier`.
    let got = dump_body("$a = ['x' => 1]; $a[$i] = 2; \\PHPStan\\dumpType($a);");
    assert!(got.contains("non-empty-array"), "expected a surviving array, got {got}");
    assert!(!got.contains("unknown"), "the barrier still ate the binding: {got}");
}

#[test]
fn a_write_at_an_unnamed_key_proves_non_empty() {
    let got = dump_decl("array<string, int>", "$v[$s] = 1; \\PHPStan\\dumpType($v);");
    assert!(got.contains("non-empty-array"), "a write leaves an entry behind: {got}");
}

// What it refuses to say

#[test]
fn list_ness_does_not_survive_an_unnamed_index() {
    // `php -r '$a=[1,2,3]; $i=7; $a[$i]=99; var_dump(array_is_list($a));'` → false.
    let got = dump_decl("list<int>", "$v[$i] = 1; \\PHPStan\\dumpType($v);");
    assert!(!got.contains("list"), "an unnamed index can break a list: {got}");
    assert!(got.contains("non-empty-array"), "…but the array itself survives: {got}");
}

#[test]
fn an_existing_slot_joins_the_written_value_rather_than_being_replaced() {
    // Which key moved is unknown, so `$a['k']` may be the slot that was
    // overwritten or the one that was not — the join says both, and says it
    // without falling to unknown.
    assert_eq!(
        dump_body("$a = ['k' => 'old']; $a[$s] = 'new'; \\PHPStan\\dumpType($a['k']);"),
        "dumped type: 'new'|'old'"
    );
}

#[test]
fn a_deeper_chain_under_an_unnamed_key_stays_a_barrier() {
    // `$a[$i]['k'] = v` — the INNER key of a depth-two path must still be
    // literal (A-G4), so this one never lowers and the barrier stands.
    let got = dump_body("$a = ['x' => 1]; $a[$i]['k'] = 2; \\PHPStan\\dumpType($a);");
    assert_eq!(got, "dumped type: unknown");
}

#[test]
fn unset_at_an_unnamed_key_keeps_the_barrier() {
    // `mark_absent` at a key nobody can name could only weaken, never remove,
    // so `unset` is deliberately left out of this lane.
    let got = dump_body("$a = ['x' => 1]; unset($a[$s]); \\PHPStan\\dumpType($a);");
    assert_eq!(got, "dumped type: unknown");
}

// The literal-key lane is untouched

#[test]
fn a_literal_key_write_still_takes_the_named_path() {
    // The precise answer #636 must not have coarsened: a named key still
    // promotes with the written value in its own slot.
    assert_eq!(
        dump_body("$a = []; $a['k'] = 'v'; \\PHPStan\\dumpType($a['k']);"),
        "dumped type: 'v'"
    );
}

#[test]
fn a_nested_literal_write_still_clears_only_the_outer_slot() {
    let got = dump_body("$a = ['x' => 1]; $a['x']['y'] = 2; \\PHPStan\\dumpType($a);");
    assert!(!got.contains("unknown type"), "the nested write still lowers: {got}");
    assert!(got.contains("array{"), "and still restores a shape: {got}");
}
