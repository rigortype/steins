//! ADR-0062 Amendment K — `$a[] = v`, the auto-index append (issue #636).
//!
//! `$a['k'] = v` has named its key since A-G8; `$a[] = v` named nothing, so it
//! lowered to `StmtKind::Barrier` and cleared the whole scope. It lowers to
//! `StmtKind::OffsetAppend` now, and the walk computes the landing index the
//! way `array_push` already does — one rule, two spellings.
//!
//! The suite is mostly about **where the index lands and when it is knowable**,
//! because that is the part PHP makes subtle. Every claim below is a
//! measurement at PHP 8.5.9, quoted at its assertion:
//!
//! * the next index is `max(integer keys) + 1`, `0` when there is none, and it
//!   counts negative keys since PHP 8.3;
//! * it is a **high-water mark**, so `unset` does not lower it — which is why
//!   the order witness is dropped by an `unset` and why an append onto a shape
//!   that lost its witness answers the weak row instead of a key;
//! * **list-ness does not survive an append** unless the exact key sequence is
//!   witnessed. `array_is_list` can say `true` about a value whose next append
//!   breaks it.
//!
//! Zero emission (A-G9): every fixture here dumps and nothing else.

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

fn one_type(src: &str) -> String {
    let tree = SourceTree::parse(src);
    let ds: Vec<Diagnostic> = check_with(&tree, &[], "t.php", &mut Mock::sidecar())
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect();
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "an append emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

fn dump_body(body: &str) -> String {
    one_type(&format!("<?php\nfunction f(int $i, string $s): void {{ {body} }}\n"))
}

fn dump_decl(decl: &str, body: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {decl} $v */\n\
         function f(array $v, int $i, string $s): void {{ {body} }}\n"
    ))
}

// Where the index lands

#[test]
fn an_append_onto_an_empty_literal_lands_at_zero() {
    // `php -r '$a=[]; $a[]="x"; var_dump(array_keys($a));'` => int(0)
    assert_eq!(dump_body("$a = []; $a[] = 'x'; \\PHPStan\\dumpType($a);"), "dumped type: list{'x'}");
}

#[test]
fn two_appends_run_the_index_forward() {
    assert_eq!(
        dump_body("$a = []; $a[] = 'x'; $a[] = 'y'; \\PHPStan\\dumpType($a);"),
        "dumped type: list{'x', 'y'}"
    );
}

#[test]
fn an_append_past_string_keys_starts_at_zero() {
    // `php -r '$a=["k"=>1]; $a[]=2; var_dump(array_keys($a));'` => 'k', int(0)
    assert_eq!(
        dump_body("$a = ['k' => 1]; $a[] = 2; \\PHPStan\\dumpType($a);"),
        "dumped type: array{k: 1, 0: 2}"
    );
}

#[test]
fn an_append_is_one_past_the_maximum_integer_key_not_the_count() {
    // `php -r '$a=[5=>1,2=>2]; $a[]=9; var_dump(array_keys($a));'` => 5, 2, 6.
    // The count is 2 and the landing index is 6, and the spelling is in the
    // witnessed build order, not the canonical sort.
    assert_eq!(
        dump_body("$a = [5 => 1, 2 => 2]; $a[] = 9; \\PHPStan\\dumpType($a);"),
        "dumped type: array{5: 1, 2: 2, 6: 9}"
    );
}

#[test]
fn an_append_counts_negative_keys() {
    // `php -r '$a=[-3=>1]; $a[]=9; var_dump(array_keys($a));'` => -3, -2.
    // PHP 8.3 changed this; before it the append landed on 0.
    assert_eq!(
        dump_body("$a = [-3 => 1]; $a[] = 9; \\PHPStan\\dumpType($a);"),
        "dumped type: array{-3: 1, -2: 9}"
    );
}

#[test]
fn an_append_onto_a_literal_list_extends_it() {
    assert_eq!(
        dump_body("$a = [1, 2, 3]; $a[] = 9; \\PHPStan\\dumpType($a);"),
        "dumped type: list{1, 2, 3, 9}"
    );
}

// The high-water mark: `unset` is the one operation that moves the index off
// `max(keys) + 1`, and it is fenced by dropping the order witness.

#[test]
fn an_append_after_an_unset_refuses_to_name_the_index() {
    // `php -r '$a=[1,2,3]; unset($a[2]); $a[]=9; var_dump(array_keys($a));'` => 0, 1, 3.
    // `max(0, 1) + 1` is 2, and PHP says 3, so the shape may not name a key.
    let got = dump_body("$a = [1, 2, 3]; unset($a[2]); $a[] = 9; \\PHPStan\\dumpType($a);");
    assert!(!got.contains("2: 9"), "the freed index is NOT reused: {got}");
    assert!(!got.contains("3: 9"), "and the shape cannot name the real one either: {got}");
    assert!(got.contains("non-empty-array"), "…but the array survives: {got}");
}

#[test]
fn an_append_onto_an_array_emptied_by_unset_is_not_a_list() {
    // `php -r '$a=[]; $a[5]=1; unset($a[5]); $a[]=2; var_dump(array_keys($a));'` => int(6).
    // The array is empty and `array_is_list([])` is true, yet the append lands
    // on 6 and the result is not a list.
    let got = dump_body("$a = []; $a[5] = 1; unset($a[5]); $a[] = 2; \\PHPStan\\dumpType($a);");
    assert!(!got.contains("list"), "an emptied array's next index is not 0: {got}");
    assert_eq!(got, "dumped type: non-empty-array<int, 2>");
}

/// The claim ADR-0062 §4 used to make and PHP refutes:
///
/// ```text
/// php -r '$x=[1,2,3]; unset($x[2]); var_dump(array_is_list($x));'          => true
/// php -r '$x=[1,2,3]; unset($x[2]); $x[]=99; var_dump(array_is_list($x));' => false
/// ```
#[test]
fn a_declared_list_does_not_stay_a_list_through_an_append() {
    let got = dump_decl("list<int>", "$v[] = 9; \\PHPStan\\dumpType($v);");
    assert!(!got.contains("list"), "a list type does not pin the next index: {got}");
    assert!(got.contains("non-empty-array"), "…but the array survives: {got}");
}

// What still declines

#[test]
fn an_append_through_a_property_stays_a_barrier() {
    // ADR-0063 §2.3 owns aliasing; the append lane takes plain locals only.
    let src = "<?php\nclass C { /** @var list<int> */ public array $p = []; }\n\
               function f(C $o): void { $a = ['k' => 1]; $o->p[] = 2; \\PHPStan\\dumpType($a); }\n";
    assert_eq!(one_type(src), "dumped type: unknown");
}

#[test]
fn a_nested_append_stays_a_barrier() {
    // `$a['k'][] = v` is a nested-shape update, which A-G8 declines.
    let got = dump_body("$a = ['k' => [1]]; $a['k'][] = 2; \\PHPStan\\dumpType($a);");
    assert_eq!(got, "dumped type: unknown");
}

#[test]
fn an_append_of_an_unproven_value_still_keeps_the_array() {
    // The slot is the unknown floor; the key set and the count are not.
    assert_eq!(
        dump_body("$a = [1]; $a[] = $i; \\PHPStan\\dumpType($a);"),
        "dumped type: list{1, int}"
    );
}
